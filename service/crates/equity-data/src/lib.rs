//! Reusable public equity reference-data and history collectors.
//!
//! Provider response formats end here. Portfolio and feature crates consume
//! only the owned records below and contain no HTTP, symbol-mapping, or vendor
//! decoding code.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use calamine::{Data, Reader, open_workbook_auto_from_rs};
use rayon::prelude::*;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};

const NASDAQ_API: &str = "https://api.nasdaq.com/api/nordic";
const NASDAQ_INDEXES: &str = "https://indexes.nasdaq.com";
const YAHOO_API: &str = "https://query1.finance.yahoo.com/v8/finance/chart";
const FI_SHORT_PAGE: &str = "https://www.fi.se/en/our-registers/net-short-positions/";
const FI_SHORT_HISTORICAL: &str = "https://www.fi.se/BlankningsRegister/GetHistFile";
const FI_SHORT_AGGREGATE: &str =
    "https://www.fi.se/BlankningsRegister/GetBlankningsregisterAggregat";
const SKV_EQUITY_HISTORY: &str = "https://www.skatteverket.se//aktiehistorik";
const SKV_ORIGIN: &str = "https://www.skatteverket.se";
const FI_PDMR_PAGE: &str = "https://www.fi.se/en/our-registers/pdmr-transactions/";
const FI_PDMR_EXPORT: &str = "https://marknadssok.fi.se/Publiceringsklient/en-GB/Search/Search";
const NASDAQ_COMPANY_NEWS_PAGE: &str =
    "https://www.nasdaq.com/european-market-activity/news/company-news";
const NASDAQ_COMPANY_NEWS_API: &str = "https://api.news.eu.nasdaq.com/news/query.action";
const NASDAQ_MARKET_NOTICES_PAGE: &str =
    "https://www.nasdaq.com/european-market-activity/news/market-notices";
const XBRL_FILINGS_API: &str = "https://filings.xbrl.org/api/filings";
const XBRL_FILINGS_ORIGIN: &str = "https://filings.xbrl.org";
const RIKSBANK_API: &str = "https://api.riksbank.se/swea/v1";
// The public hostname currently has an official Azure API Management alias.
// Retain it only as a transport fallback for resolver failures; dataset source
// attribution remains Sveriges Riksbank.
const RIKSBANK_API_FALLBACK: &str = "https://apimgmt-prod1365.azure-api.net/swea/v1";
const EODHD_API: &str = "https://eodhd.com/api";
const USER_AGENT: &str = "ai-trader-stockholm-research/0.1";
const PDF_EXTRACTOR_ADDRESS_SPACE_BYTES: u64 = 512 * 1024 * 1024;
const PDF_EXTRACTOR_TIMEOUT_SECONDS: u64 = 120;

fn default_true() -> bool {
    true
}

pub const RIKSBANK_STOCKHOLM_MACRO_SERIES: &[(&str, &str, &str)] = &[
    ("SEKUSDPMI", "SEK per US dollar", "16:15 Europe/Stockholm"),
    ("SEKEURPMI", "SEK per euro", "16:15 Europe/Stockholm"),
    (
        "SEKKIX92",
        "Swedish KIX effective exchange-rate index",
        "16:15 Europe/Stockholm",
    ),
    (
        "SECBREPOEFF",
        "Riksbank policy rate",
        "09:10 Europe/Stockholm",
    ),
];

pub const NASDAQ_FINANCIAL_REPORT_CATEGORIES: &[&str] = &[
    "Annual financial report",
    "Annual Financial Report",
    "Annual report",
    "Annual report/ annual accounts",
    "Financial statement release",
    "Financial Statement Release",
    "Half year financial report",
    "Half Year financial report",
    "Interim information",
    "Interim Management statement",
    "Interim report (Q1 and Q3)",
    "Quarterly report",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UniverseBucket {
    LargeCap,
    MidCap,
    SmallCap,
    FirstNorthPremier,
    FirstNorth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instrument {
    pub orderbook_id: String,
    pub isin: String,
    pub symbol: String,
    pub name: String,
    pub currency: String,
    pub sector: String,
    pub bucket: UniverseBucket,
    pub yahoo_symbol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub adjusted_close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentHistory {
    pub instrument: Instrument,
    pub bars: Vec<DailyBar>,
}

/// Official Nasdaq Nordic end-of-session market fields. These are raw,
/// unadjusted exchange observations and therefore complement rather than
/// replace a corporate-action-adjusted total-return series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqDailyMarketBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub average: Option<f64>,
    pub total_volume: f64,
    pub turnover_sek: f64,
    pub trades: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqInstrumentMarketHistory {
    pub instrument: Instrument,
    pub source: String,
    #[serde(with = "date_serde")]
    pub requested_start: Date,
    #[serde(with = "date_serde")]
    pub requested_end: Date,
    pub source_rows: usize,
    pub rejected_rows: usize,
    pub bars: Vec<NasdaqDailyMarketBar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqMarketHistoryManifest {
    pub format_version: String,
    pub generated_at: String,
    #[serde(with = "date_serde")]
    pub requested_start: Date,
    #[serde(with = "date_serde")]
    pub requested_end: Date,
    pub universe_source: String,
    pub history_source: String,
    pub survivorship_status: String,
    pub instruments_discovered: usize,
    #[serde(default)]
    pub supplemental_instruments: usize,
    pub instruments_requested: usize,
    pub instruments_with_history: usize,
    pub instruments_failed: usize,
    pub bars: usize,
    #[serde(default)]
    pub source_rows: usize,
    #[serde(default)]
    pub rejected_rows: usize,
    #[serde(default)]
    pub bars_with_two_sided_quote: usize,
    #[serde(default)]
    pub bars_with_trade_count: usize,
    pub earliest_bar: Option<String>,
    pub latest_bar: Option<String>,
    pub failures: BTreeMap<String, String>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct NasdaqMarketHistoryCollection {
    pub dataset_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub instruments: usize,
    pub bars: usize,
    pub failures: usize,
}

/// One official Nasdaq index session. `start_value` is Nasdaq's SOD level and
/// `end_value` is its EOD level. For a gross-return index these levels include
/// the index provider's corporate-action and dividend treatment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub start_value: f64,
    pub end_value: f64,
    pub high_value: Option<f64>,
    pub low_value: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkHistory {
    pub format_version: String,
    pub symbol: String,
    pub name: String,
    pub return_type: String,
    pub currency: String,
    pub source: String,
    pub generated_at: String,
    pub bars: Vec<BenchmarkBar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetManifest {
    pub format_version: String,
    pub generated_at: String,
    #[serde(with = "date_serde")]
    pub requested_start: Date,
    #[serde(with = "date_serde")]
    pub requested_end: Date,
    pub universe_source: String,
    pub history_source: String,
    pub survivorship_status: String,
    pub instruments_discovered: usize,
    pub instruments_with_history: usize,
    pub instruments_failed: usize,
    pub bars: usize,
    pub failures: BTreeMap<String, String>,
}

/// A public net-short position notification. FI's historical workbook only
/// exposes positions at or above 0.5% of issued share capital. A row rendered
/// as `<0.5` is a threshold-exit notification, not a measured zero position.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FiHistoricalNetShortPosition {
    pub holder: String,
    pub issuer: String,
    pub isin: String,
    pub position_percent: Option<f64>,
    pub below_half_percent: bool,
    #[serde(with = "date_serde")]
    pub position_date: Date,
    pub comment: Option<String>,
}

/// FI's latest aggregate net-short interest. Aggregate publication starts at
/// 0.1%, but no holder identities are supplied below the 0.5% public threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FiAggregateNetShortPosition {
    pub issuer: String,
    pub lei: String,
    pub position_percent: f64,
    #[serde(with = "date_serde")]
    pub latest_position_date: Date,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FiNetShortDataset {
    pub format_version: String,
    pub generated_at: String,
    pub source_page: String,
    pub historical_source: String,
    pub aggregate_source: String,
    pub raw_historical_file: String,
    pub raw_aggregate_file: String,
    pub limitations: Vec<String>,
    pub historical: Vec<FiHistoricalNetShortPosition>,
    pub aggregate: Vec<FiAggregateNetShortPosition>,
}

#[derive(Debug, Clone)]
pub struct FiNetShortCollection {
    pub snapshot_dir: PathBuf,
    pub dataset_path: PathBuf,
    pub historical_positions: usize,
    pub aggregate_positions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkvEquityHistoryCompany {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkvEquityHistorySourcePage {
    pub url: String,
    pub archive_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkvEquityHistoryCatalogue {
    pub format_version: String,
    pub generated_at: String,
    pub source: String,
    pub source_pages: Vec<SkvEquityHistorySourcePage>,
    pub limitations: Vec<String>,
    pub companies: Vec<SkvEquityHistoryCompany>,
}

#[derive(Debug, Clone)]
pub struct SkvEquityHistoryCollection {
    pub snapshot_dir: PathBuf,
    pub catalogue_path: PathBuf,
    pub companies: usize,
    pub source_pages: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkvListingEventKind {
    Listing,
    Delisting,
    ListChange,
    Status,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkvMarketHint {
    StockholmMainMarket,
    FirstNorth,
    OtherSwedishVenue,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkvListingHistoryRow {
    pub company_name: String,
    pub source_url: String,
    pub year: Option<i32>,
    #[serde(default, with = "optional_date_serde")]
    pub effective_date: Option<Date>,
    pub comment: String,
    pub event_kind: SkvListingEventKind,
    pub market_hint: SkvMarketHint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkvListingHistoryDataset {
    pub format_version: String,
    pub generated_at: String,
    pub source_catalogue: String,
    pub raw_cache_dir: String,
    pub pause_ms: u64,
    pub companies_requested: usize,
    pub companies_archived: usize,
    pub failures: BTreeMap<String, String>,
    pub limitations: Vec<String>,
    pub rows: Vec<SkvListingHistoryRow>,
}

#[derive(Debug, Clone)]
pub struct SkvListingHistoryCollection {
    pub dataset_path: PathBuf,
    pub companies_requested: usize,
    pub companies_archived: usize,
    pub listing_rows: usize,
    pub failures: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FiPdmrTransaction {
    #[serde(with = "date_serde")]
    pub publication_date: Date,
    pub publication_time: String,
    pub issuer: String,
    pub lei: String,
    pub notifier: String,
    pub pdmr: String,
    pub position: String,
    pub closely_associated: bool,
    pub amendment: bool,
    pub amendment_details: Option<String>,
    pub initial_notification: bool,
    pub linked_to_share_option_programme: bool,
    pub nature: String,
    pub instrument_type: String,
    pub instrument_name: String,
    pub isin: Option<String>,
    #[serde(with = "date_serde")]
    pub transaction_date: Date,
    pub volume: Option<f64>,
    pub unit: String,
    pub price: Option<f64>,
    pub currency: String,
    pub trading_venue: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FiPdmrDataset {
    pub format_version: String,
    pub generated_at: String,
    pub source_page: String,
    pub export_endpoint: String,
    #[serde(with = "date_serde")]
    pub requested_start: Date,
    #[serde(with = "date_serde")]
    pub requested_end: Date,
    pub query_basis: String,
    pub interval_days: usize,
    pub pause_ms: u64,
    pub raw_cache_dir: String,
    pub limitations: Vec<String>,
    pub transactions: Vec<FiPdmrTransaction>,
}

#[derive(Debug, Clone)]
pub struct FiPdmrCollection {
    pub dataset_path: PathBuf,
    pub transactions: usize,
    pub unique_isins: usize,
    pub intervals: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqCompanyAnnouncement {
    pub disclosure_id: u64,
    pub category_id: u64,
    pub category: String,
    pub headline: String,
    pub language: String,
    pub message_url: String,
    pub published: String,
    #[serde(with = "date_serde")]
    pub publication_date: Date,
    pub market: String,
    pub company: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqCompanyNewsDataset {
    pub format_version: String,
    pub generated_at: String,
    pub source_page: String,
    pub query_endpoint: String,
    #[serde(with = "date_serde")]
    pub requested_start: Date,
    #[serde(with = "date_serde")]
    pub requested_end: Date,
    pub market: String,
    pub categories: Vec<String>,
    pub pause_ms: u64,
    pub raw_cache_dir: String,
    pub limitations: Vec<String>,
    pub announcements: Vec<NasdaqCompanyAnnouncement>,
}

#[derive(Debug, Clone)]
pub struct NasdaqCompanyNewsCollection {
    pub dataset_path: PathBuf,
    pub announcements: usize,
    pub companies: usize,
    pub requests: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqNewsAttachment {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqFinancialReportMessage {
    pub announcement: NasdaqCompanyAnnouncement,
    pub body_text: String,
    pub attachments: Vec<NasdaqNewsAttachment>,
    pub raw_message_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqFinancialReportMessageDataset {
    pub format_version: String,
    pub generated_at: String,
    pub source_page: String,
    pub metadata_source: String,
    pub pause_ms: u64,
    pub concurrency: usize,
    pub raw_cache_dir: String,
    pub requested_messages: usize,
    pub limitations: Vec<String>,
    pub message_failures: BTreeMap<u64, String>,
    pub messages: Vec<NasdaqFinancialReportMessage>,
}

#[derive(Debug, Clone)]
pub struct NasdaqFinancialReportMessageCollection {
    pub dataset_path: PathBuf,
    pub requested: usize,
    pub messages: usize,
    pub attachments: usize,
    pub failures: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqFinancialReportAttachmentDocument {
    pub url: String,
    pub names: Vec<String>,
    pub disclosure_ids: Vec<u64>,
    pub byte_length: Option<u64>,
    pub raw_file: Option<String>,
    pub extracted_text_file: Option<String>,
    pub extracted_text_chars: usize,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqFinancialReportAttachmentDataset {
    pub format_version: String,
    pub generated_at: String,
    pub source: String,
    pub message_metadata_source: String,
    pub pause_ms: u64,
    pub concurrency: usize,
    pub max_attachment_bytes: u64,
    #[serde(default = "default_true")]
    pub network_downloads_enabled: bool,
    pub raw_cache_dir: String,
    pub text_cache_dir: String,
    pub available_pdf_urls: usize,
    pub requested_pdf_urls: usize,
    pub limitations: Vec<String>,
    pub documents: Vec<NasdaqFinancialReportAttachmentDocument>,
}

#[derive(Debug, Clone)]
pub struct NasdaqFinancialReportAttachmentCollection {
    pub dataset_path: PathBuf,
    pub available: usize,
    pub requested: usize,
    pub downloaded: usize,
    pub extracted: usize,
    pub bytes: u64,
    pub text_chars: usize,
    pub failures: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EodhdDelistedSymbol {
    pub code: String,
    pub name: String,
    pub exchange: String,
    pub currency: String,
    pub security_type: String,
    pub isin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EodhdDailyBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub adjusted_close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EodhdDelistedHistory {
    pub symbol: EodhdDelistedSymbol,
    pub official_notice_ids: Vec<u64>,
    #[serde(default, with = "optional_date_serde")]
    pub official_last_trading_date: Option<Date>,
    pub bars: Vec<EodhdDailyBar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EodhdStockholmDelistedDataset {
    pub format_version: String,
    pub generated_at: String,
    pub provider: String,
    pub exchange_code: String,
    pub operating_mic: String,
    pub official_notice_source: String,
    #[serde(with = "date_serde")]
    pub requested_start: Date,
    #[serde(with = "date_serde")]
    pub requested_end: Date,
    pub raw_cache_dir: String,
    pub limitations: Vec<String>,
    pub provider_delisted_symbols: usize,
    pub official_delisting_isins: usize,
    pub matched_isins: usize,
    pub failures: BTreeMap<String, String>,
    pub histories: Vec<EodhdDelistedHistory>,
}

#[derive(Debug, Clone)]
pub struct EodhdStockholmDelistedCollection {
    pub dataset_path: PathBuf,
    pub provider_symbols: usize,
    pub official_isins: usize,
    pub matched_isins: usize,
    pub histories: usize,
    pub bars: usize,
    pub failures: usize,
}

/// One provider-normalized quarterly filing. Values remain statement units;
/// price joins and ratios belong to the Rust feature crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EodhdQuarterlyFundamental {
    #[serde(with = "date_serde")]
    pub report_period_end: Date,
    /// Provider filing date. This, rather than the accounting period end, is
    /// the earliest date on which the row may enter a causal feature matrix.
    #[serde(with = "date_serde")]
    pub available_date: Date,
    pub filing_key: String,
    pub values: AnnualFundamentals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EodhdFundamentalHistory {
    pub symbol: EodhdDelistedSymbol,
    pub quarterly: Vec<EodhdQuarterlyFundamental>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EodhdStockholmFundamentalDataset {
    pub format_version: String,
    pub generated_at: String,
    pub provider: String,
    pub endpoint: String,
    pub exchange_code: String,
    pub operating_mic: String,
    pub universe_source: String,
    pub official_notice_source: String,
    pub raw_cache_dir: String,
    pub pause_ms: u64,
    pub limitations: Vec<String>,
    pub provider_symbols: usize,
    pub target_isins: usize,
    pub matched_isins: usize,
    pub failures: BTreeMap<String, String>,
    pub histories: Vec<EodhdFundamentalHistory>,
}

#[derive(Debug, Clone)]
pub struct EodhdStockholmFundamentalCollection {
    pub dataset_path: PathBuf,
    pub provider_symbols: usize,
    pub target_isins: usize,
    pub matched_isins: usize,
    pub histories: usize,
    pub quarterly_filings: usize,
    pub failures: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NasdaqEquityNoticeKind {
    Listing,
    Delisting,
    IdentityChange,
    SegmentChange,
    Suspension,
    Resumption,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqEquityNotice {
    pub disclosure_id: u64,
    pub headline: String,
    pub message_url: String,
    pub published: String,
    #[serde(with = "date_serde")]
    pub publication_date: Date,
    pub event_kind: NasdaqEquityNoticeKind,
    pub body_mentions_stockholm: bool,
    pub short_names: Vec<String>,
    pub isins: Vec<String>,
    pub orderbook_ids: Vec<String>,
    #[serde(default, with = "optional_date_serde")]
    pub first_trading_date: Option<Date>,
    #[serde(default, with = "optional_date_serde")]
    pub last_trading_date: Option<Date>,
    pub raw_message_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NasdaqEquityNoticeDataset {
    pub format_version: String,
    pub generated_at: String,
    pub source_page: String,
    pub query_endpoint: String,
    #[serde(with = "date_serde")]
    pub requested_start: Date,
    #[serde(with = "date_serde")]
    pub requested_end: Date,
    pub category: String,
    pub pause_ms: u64,
    pub raw_cache_dir: String,
    pub limitations: Vec<String>,
    pub metadata_notices_seen: usize,
    pub candidate_messages: usize,
    pub message_failures: BTreeMap<u64, String>,
    pub notices: Vec<NasdaqEquityNotice>,
}

#[derive(Debug, Clone)]
pub struct NasdaqEquityNoticeCollection {
    pub dataset_path: PathBuf,
    pub metadata_notices_seen: usize,
    pub candidate_messages: usize,
    pub notices: usize,
    pub identifiers: usize,
    pub failures: usize,
    pub requests: usize,
}

/// A numeric, non-dimensional IFRS fact from an ESEF annual report. XBRL's
/// end timestamps are exclusive, so `period_end` is normalized to the final
/// covered calendar date before this record leaves the provider crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EsefIfrsFact {
    pub concept: String,
    #[serde(default, with = "optional_date_serde")]
    pub period_start: Option<Date>,
    #[serde(with = "date_serde")]
    pub period_end: Date,
    pub unit: String,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EsefAnnualFiling {
    pub filing_id: String,
    pub entity_name: String,
    pub lei: String,
    #[serde(with = "date_serde")]
    pub report_period_end: Date,
    #[serde(with = "date_serde")]
    pub repository_date_added: Date,
    #[serde(default, with = "optional_date_serde")]
    pub official_annual_report_date: Option<Date>,
    /// Conservative causal boundary: the later of the repository ingestion
    /// date and a matched official Nasdaq annual-report announcement.
    #[serde(with = "date_serde")]
    pub available_date: Date,
    pub json_url: String,
    pub package_url: String,
    pub sha256: String,
    pub error_count: usize,
    pub warning_count: usize,
    pub inconsistency_count: usize,
    pub facts: Vec<EsefIfrsFact>,
}

/// Provider-neutral annual statement fields selected from standard IFRS
/// concepts. No ratios or model features are calculated here; this struct is
/// the stable hand-off from the ESEF data source to feature construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnualFundamentals {
    pub reporting_currency: Option<String>,
    pub revenue: Option<f64>,
    pub prior_revenue: Option<f64>,
    pub operating_profit: Option<f64>,
    pub net_income: Option<f64>,
    pub prior_net_income: Option<f64>,
    pub assets: Option<f64>,
    pub prior_assets: Option<f64>,
    pub equity: Option<f64>,
    pub prior_equity: Option<f64>,
    pub cash: Option<f64>,
    pub operating_cash_flow: Option<f64>,
    pub current_assets: Option<f64>,
    pub current_liabilities: Option<f64>,
    pub basic_eps: Option<f64>,
    pub weighted_average_shares: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EsefAnnualDataset {
    pub format_version: String,
    pub generated_at: String,
    pub source: String,
    pub api_endpoint: String,
    pub country: String,
    pub raw_cache_dir: String,
    pub pause_ms: u64,
    pub limitations: Vec<String>,
    pub filings_seen: usize,
    pub filings_without_json: usize,
    pub filings_failed: BTreeMap<String, String>,
    pub filings: Vec<EsefAnnualFiling>,
}

#[derive(Debug, Clone)]
pub struct EsefAnnualCollection {
    pub dataset_path: PathBuf,
    pub filings_seen: usize,
    pub filings_parsed: usize,
    pub entities: usize,
    pub facts: usize,
    pub failures: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiksbankObservation {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiksbankSeries {
    pub series_id: String,
    pub description: String,
    pub publication_time: String,
    pub observations: Vec<RiksbankObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiksbankMacroDataset {
    pub format_version: String,
    pub generated_at: String,
    pub source: String,
    pub api_endpoint: String,
    #[serde(with = "date_serde")]
    pub requested_start: Date,
    #[serde(with = "date_serde")]
    pub requested_end: Date,
    pub raw_cache_dir: String,
    pub limitations: Vec<String>,
    pub series: Vec<RiksbankSeries>,
}

#[derive(Debug, Clone)]
pub struct RiksbankMacroCollection {
    pub dataset_path: PathBuf,
    pub series: usize,
    pub observations: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RiksbankApiObservation {
    date: String,
    value: f64,
}

#[derive(Debug, Deserialize)]
struct XbrlApiResponse {
    data: Vec<XbrlApiFiling>,
    #[serde(default)]
    included: Vec<XbrlApiEntity>,
    meta: XbrlApiMeta,
}

#[derive(Debug, Deserialize)]
struct XbrlApiMeta {
    count: usize,
}

#[derive(Debug, Deserialize)]
struct XbrlApiFiling {
    id: String,
    attributes: XbrlApiFilingAttributes,
    relationships: XbrlApiFilingRelationships,
}

#[derive(Debug, Deserialize)]
struct XbrlApiFilingAttributes {
    period_end: String,
    date_added: String,
    json_url: Option<String>,
    package_url: String,
    sha256: String,
    error_count: usize,
    warning_count: usize,
    inconsistency_count: usize,
}

#[derive(Debug, Deserialize)]
struct XbrlApiFilingRelationships {
    entity: XbrlApiEntityRelationship,
}

#[derive(Debug, Deserialize)]
struct XbrlApiEntityRelationship {
    data: XbrlApiResourceIdentifier,
}

#[derive(Debug, Deserialize)]
struct XbrlApiResourceIdentifier {
    id: String,
}

#[derive(Debug, Deserialize)]
struct XbrlApiEntity {
    id: String,
    attributes: XbrlApiEntityAttributes,
}

#[derive(Debug, Clone, Deserialize)]
struct XbrlApiEntityAttributes {
    name: String,
    identifier: String,
}

#[derive(Debug, Deserialize)]
struct XbrlJsonDocument {
    facts: BTreeMap<String, XbrlJsonFact>,
}

#[derive(Debug, Deserialize)]
struct XbrlJsonFact {
    value: Option<serde_json::Value>,
    dimensions: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct NasdaqCompanyNewsResponse {
    results: NasdaqCompanyNewsResults,
    count: usize,
}

#[derive(Debug, Deserialize)]
struct NasdaqCompanyNewsResults {
    item: Vec<NasdaqCompanyNewsItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NasdaqCompanyNewsItem {
    disclosure_id: u64,
    category_id: u64,
    headline: String,
    language: String,
    cns_category: String,
    message_url: String,
    published: String,
    market: String,
    company: String,
}

#[derive(Debug, Deserialize)]
struct NasdaqResponse {
    data: NasdaqData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NasdaqData {
    instrument_listing: NasdaqListing,
}

#[derive(Debug, Deserialize)]
struct NasdaqListing {
    rows: Vec<NasdaqRow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NasdaqRow {
    full_name: String,
    currency: String,
    orderbook_id: String,
    symbol: String,
    sector: String,
    isin: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NasdaqMarketHistoryResponse {
    data: Option<NasdaqMarketHistoryData>,
    messages: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NasdaqMarketHistoryData {
    chart_data: NasdaqMarketChartData,
    charts: NasdaqMarketCharts,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NasdaqMarketChartData {
    orderbook_id: String,
    asset_class: String,
    isin: String,
    symbol: String,
}

#[derive(Debug, Deserialize)]
struct NasdaqMarketCharts {
    rows: Vec<NasdaqMarketHistoryRow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NasdaqMarketHistoryRow {
    date_time: String,
    #[serde(default)]
    bid: String,
    #[serde(default)]
    ask: String,
    #[serde(default)]
    open: String,
    #[serde(default)]
    high: String,
    #[serde(default)]
    low: String,
    #[serde(default)]
    close: String,
    #[serde(default)]
    average: String,
    #[serde(default)]
    total_volume: String,
    #[serde(default)]
    turnover: String,
    #[serde(default)]
    trades: String,
}

#[derive(Debug, Deserialize)]
struct YahooResponse {
    chart: YahooChart,
}

#[derive(Debug, Deserialize)]
struct YahooChart {
    result: Option<Vec<YahooResult>>,
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct YahooResult {
    timestamp: Vec<i64>,
    indicators: YahooIndicators,
}

#[derive(Debug, Deserialize)]
struct YahooIndicators {
    quote: Vec<YahooQuote>,
    adjclose: Option<Vec<YahooAdjusted>>,
}

#[derive(Debug, Deserialize)]
struct YahooQuote {
    open: Vec<Option<f64>>,
    high: Vec<Option<f64>>,
    low: Vec<Option<f64>>,
    close: Vec<Option<f64>>,
    volume: Vec<Option<f64>>,
}

#[derive(Debug, Deserialize)]
struct YahooAdjusted {
    adjclose: Vec<Option<f64>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NasdaqIndexResponse {
    #[serde(rename = "aaData")]
    rows: Vec<NasdaqIndexRow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct NasdaqIndexRow {
    time_stamp: String,
    value: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    currency: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EodhdSymbolResponse {
    code: String,
    name: String,
    exchange: String,
    currency: String,
    #[serde(rename = "Type")]
    security_type: String,
    isin: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EodhdBarResponse {
    date: String,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    adjusted_close: f64,
    volume: f64,
}

pub struct PublicEquityData {
    client: reqwest::blocking::Client,
}

impl PublicEquityData {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client })
    }

    /// Current Nasdaq Stockholm Main Market and First North Sweden ordinary
    /// share lines. This is deliberately a current snapshot, not reconstructed
    /// historical membership.
    pub fn stockholm_universe(&self) -> Result<Vec<Instrument>, String> {
        let queries = [
            ("MAIN_MARKET", "LARGE_CAP", UniverseBucket::LargeCap),
            ("MAIN_MARKET", "MID_CAP", UniverseBucket::MidCap),
            ("MAIN_MARKET", "SMALL_CAP", UniverseBucket::SmallCap),
            (
                "FIRST_NORTH",
                "FN_PREMIER",
                UniverseBucket::FirstNorthPremier,
            ),
            ("FIRST_NORTH", "FN_GM", UniverseBucket::FirstNorth),
        ];
        let mut instruments = Vec::new();
        for (category, segment, bucket) in queries {
            let response: NasdaqResponse = self
                .client
                .get(format!("{NASDAQ_API}/screener/shares"))
                .query(&[
                    ("category", category),
                    ("market", "STO"),
                    ("segment", segment),
                    ("tableonly", "false"),
                ])
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .map_err(|e| format!("Nasdaq {category}/{segment}: {e}"))?
                .json()
                .map_err(|e| format!("Nasdaq {category}/{segment} response: {e}"))?;
            instruments.extend(
                response
                    .data
                    .instrument_listing
                    .rows
                    .into_iter()
                    .filter_map(|row| {
                        if row.orderbook_id.is_empty()
                            || row.isin.is_empty()
                            || row.symbol.is_empty()
                            || row.currency != "SEK"
                        {
                            return None;
                        }
                        let yahoo_symbol = yahoo_symbol(&row.symbol);
                        Some(Instrument {
                            orderbook_id: row.orderbook_id,
                            isin: row.isin,
                            symbol: row.symbol,
                            name: row.full_name,
                            currency: row.currency,
                            sector: row.sector,
                            bucket,
                            yahoo_symbol,
                        })
                    }),
            );
        }
        instruments.sort_by(|a, b| a.orderbook_id.cmp(&b.orderbook_id));
        instruments.dedup_by(|a, b| a.orderbook_id == b.orderbook_id);
        Ok(instruments)
    }

    pub fn yahoo_history(
        &self,
        instrument: &Instrument,
        start: Date,
        end: Date,
    ) -> Result<Vec<DailyBar>, String> {
        let period1 = unix_midnight(start)?;
        let period2 = unix_midnight(end.next_day().unwrap_or(end))?;
        let response: YahooResponse = self
            .client
            .get(format!("{YAHOO_API}/{}", instrument.yahoo_symbol))
            .query(&[
                ("period1", period1.to_string()),
                ("period2", period2.to_string()),
                ("interval", "1d".into()),
                ("events", "div,splits".into()),
            ])
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|e| format!("Yahoo {}: {e}", instrument.yahoo_symbol))?
            .json()
            .map_err(|e| format!("Yahoo {} response: {e}", instrument.yahoo_symbol))?;
        if let Some(error) = response.chart.error {
            return Err(format!("Yahoo {}: {error}", instrument.yahoo_symbol));
        }
        let result = response
            .chart
            .result
            .and_then(|mut values| values.pop())
            .ok_or_else(|| format!("Yahoo {} returned no chart", instrument.yahoo_symbol))?;
        let quote = result
            .indicators
            .quote
            .first()
            .ok_or_else(|| format!("Yahoo {} returned no quotes", instrument.yahoo_symbol))?;
        let adjusted = result
            .indicators
            .adjclose
            .as_ref()
            .and_then(|values| values.first())
            .ok_or_else(|| {
                format!(
                    "Yahoo {} returned no adjusted close",
                    instrument.yahoo_symbol
                )
            })?;
        let width = [
            result.timestamp.len(),
            quote.open.len(),
            quote.high.len(),
            quote.low.len(),
            quote.close.len(),
            quote.volume.len(),
            adjusted.adjclose.len(),
        ]
        .into_iter()
        .min()
        .unwrap_or(0);
        let mut bars = Vec::with_capacity(width);
        for index in 0..width {
            let values = (
                quote.open[index],
                quote.high[index],
                quote.low[index],
                quote.close[index],
                quote.volume[index],
                adjusted.adjclose[index],
            );
            let (Some(open), Some(high), Some(low), Some(close), Some(volume), Some(adj)) = values
            else {
                continue;
            };
            if ![open, high, low, close, adj]
                .iter()
                .all(|v| v.is_finite() && *v > 0.0)
                || !volume.is_finite()
                || volume < 0.0
                || high < low
                || open > high
                || open < low
                || close > high
                || close < low
            {
                continue;
            }
            let date = OffsetDateTime::from_unix_timestamp(result.timestamp[index])
                .map_err(|e| e.to_string())?
                .date();
            bars.push(DailyBar {
                date,
                open,
                high,
                low,
                close,
                adjusted_close: adj,
                volume,
            });
        }
        bars.sort_by_key(|bar| bar.date);
        bars.dedup_by_key(|bar| bar.date);
        if bars.len() < 30 {
            return Err(format!(
                "Yahoo {} returned only {} valid daily bars",
                instrument.yahoo_symbol,
                bars.len()
            ));
        }
        Ok(bars)
    }

    /// Fetch official Nasdaq Nordic daily market history for one currently
    /// listed share. The free endpoint exposes at most approximately ten
    /// years and drops inactive instruments, so callers must not mistake it
    /// for a survivorship-safe price source.
    pub fn nasdaq_market_history(
        &self,
        instrument: &Instrument,
        start: Date,
        end: Date,
    ) -> Result<Vec<NasdaqDailyMarketBar>, String> {
        if end < start {
            return Err("Nasdaq market-history end precedes start".into());
        }
        let url = format!(
            "{NASDAQ_API}/instruments/{}/chart/download",
            instrument.orderbook_id
        );
        let start_query = start.to_string();
        let end_query = end.to_string();
        let bytes = self
            .client
            .get(&url)
            .query(&[
                ("type", "TABLE_VIEW"),
                ("assetClass", "SHARES"),
                ("fromDate", start_query.as_str()),
                ("toDate", end_query.as_str()),
            ])
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| format!("Nasdaq {}: {error}", instrument.orderbook_id))?
            .bytes()
            .map_err(|error| format!("Nasdaq {} response: {error}", instrument.orderbook_id))?;
        parse_nasdaq_market_history(instrument, &bytes, start, end).map(|history| history.bars)
    }

    /// Resumably archive the official Nasdaq daily market fields for the
    /// current Stockholm Main Market universe. First North is excluded. Raw
    /// provider responses are retained so normalization can be reproduced.
    pub fn collect_nasdaq_stockholm_market_history(
        &self,
        root: &Path,
        start: Date,
        end: Date,
        pause_ms: u64,
        limit: usize,
        supplemental_universe: &[Instrument],
    ) -> Result<NasdaqMarketHistoryCollection, String> {
        if end < start {
            return Err("Nasdaq market-history end precedes start".into());
        }
        let dataset_dir = root.join("nasdaq-market-history");
        let raw_dir = dataset_dir.join("raw");
        let bars_dir = dataset_dir.join("bars");
        std::fs::create_dir_all(&raw_dir).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&bars_dir).map_err(|error| error.to_string())?;

        let current_universe = self.stockholm_universe()?;
        let current_ids = current_universe
            .iter()
            .map(|instrument| instrument.orderbook_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut universe = current_universe
            .into_iter()
            .chain(supplemental_universe.iter().cloned())
            .filter(|instrument| {
                matches!(
                    instrument.bucket,
                    UniverseBucket::LargeCap | UniverseBucket::MidCap | UniverseBucket::SmallCap
                )
            })
            .collect::<Vec<_>>();
        universe.sort_by(|left, right| left.orderbook_id.cmp(&right.orderbook_id));
        universe.dedup_by(|left, right| left.orderbook_id == right.orderbook_id);
        let supplemental_instruments = universe
            .iter()
            .filter(|instrument| !current_ids.contains(&instrument.orderbook_id))
            .count();
        let instruments_discovered = universe.len();
        if limit > 0 {
            universe.truncate(limit);
        }

        let mut failures = BTreeMap::new();
        let mut histories = Vec::new();
        for (index, instrument) in universe.iter().enumerate() {
            let raw_path = raw_dir.join(format!("{}-{start}_{end}.json", instrument.orderbook_id));
            let result = if raw_path.exists() {
                std::fs::read(&raw_path)
                    .map_err(|error| format!("{}: {error}", raw_path.display()))
                    .and_then(|bytes| parse_nasdaq_market_history(instrument, &bytes, start, end))
            } else {
                let url = format!(
                    "{NASDAQ_API}/instruments/{}/chart/download",
                    instrument.orderbook_id
                );
                let bytes = self
                    .download_bytes_with_query_retries(
                        &url,
                        &[
                            ("type", "TABLE_VIEW".into()),
                            ("assetClass", "SHARES".into()),
                            ("fromDate", start.to_string()),
                            ("toDate", end.to_string()),
                        ],
                        4,
                        2,
                    )
                    .and_then(|bytes| {
                        // Validate before caching so a transient HTML or error
                        // body cannot poison a resumed collection.
                        let bars = parse_nasdaq_market_history(instrument, &bytes, start, end)?;
                        std::fs::write(&raw_path, bytes)
                            .map_err(|error| format!("{}: {error}", raw_path.display()))?;
                        Ok(bars)
                    });
                if pause_ms > 0 {
                    std::thread::sleep(Duration::from_millis(pause_ms));
                }
                bytes
            };
            match result {
                Ok(parsed) => histories.push(NasdaqInstrumentMarketHistory {
                    instrument: instrument.clone(),
                    source: format!(
                        "{NASDAQ_API}/instruments/{}/chart/download",
                        instrument.orderbook_id
                    ),
                    requested_start: start,
                    requested_end: end,
                    source_rows: parsed.source_rows,
                    rejected_rows: parsed.rejected_rows,
                    bars: parsed.bars,
                }),
                Err(error) => {
                    failures.insert(instrument.orderbook_id.clone(), error);
                }
            }
            if (index + 1) % 25 == 0 || index + 1 == universe.len() {
                eprintln!(
                    "Nasdaq market history: {}/{}, {} histories, {} failures",
                    index + 1,
                    universe.len(),
                    histories.len(),
                    failures.len()
                );
            }
        }
        histories.sort_by(|left, right| {
            left.instrument
                .orderbook_id
                .cmp(&right.instrument.orderbook_id)
        });
        for history in &histories {
            write_json(
                &bars_dir.join(format!("{}.json", history.instrument.orderbook_id)),
                history,
            )?;
        }

        let earliest_bar = histories
            .iter()
            .filter_map(|history| history.bars.first().map(|bar| bar.date))
            .min()
            .map(|date| date.to_string());
        let latest_bar = histories
            .iter()
            .filter_map(|history| history.bars.last().map(|bar| bar.date))
            .max()
            .map(|date| date.to_string());
        let now = OffsetDateTime::now_utc();
        let manifest = NasdaqMarketHistoryManifest {
            format_version: "nasdaq-stockholm-market-history-1".into(),
            generated_at: now.to_string(),
            requested_start: start,
            requested_end: end,
            universe_source: "Nasdaq Nordic screener current STO Main Market Large/Mid/Small Cap segments plus explicitly supplied prior snapshots".into(),
            history_source: format!(
                "{NASDAQ_API}/instruments/{{orderBookID}}/chart/download"
            ),
            survivorship_status: "SURVIVORSHIP_CONTAMINATED".into(),
            instruments_discovered,
            supplemental_instruments,
            instruments_requested: universe.len(),
            instruments_with_history: histories.len(),
            instruments_failed: failures.len(),
            bars: histories.iter().map(|history| history.bars.len()).sum(),
            source_rows: histories.iter().map(|history| history.source_rows).sum(),
            rejected_rows: histories.iter().map(|history| history.rejected_rows).sum(),
            bars_with_two_sided_quote: histories
                .iter()
                .flat_map(|history| &history.bars)
                .filter(|bar| bar.bid.is_some() && bar.ask.is_some())
                .count(),
            bars_with_trade_count: histories
                .iter()
                .flat_map(|history| &history.bars)
                .filter(|bar| bar.trades.is_some())
                .count(),
            earliest_bar,
            latest_bar,
            failures,
            limitations: vec![
                "The public endpoint exposes only currently resolvable instrument IDs; delisted instruments return Instrument not found".into(),
                "The public endpoint currently truncates requests to approximately ten years even when an earlier start is requested".into(),
                "Prices are raw and unadjusted; this dataset has no dividend or split-adjusted total-return field".into(),
                "Bid and ask are end-of-session snapshots, not intraday execution quotes; causal consumers must lag them and retain a conservative spread floor".into(),
                "Incomplete or internally invalid OHLC rows are counted and omitted rather than imputed; malformed numeric fields fail the instrument".into(),
                "This archive is useful for observed liquidity, traded value, trade count, and source auditing but cannot clear the survivorship gate".into(),
            ],
        };
        let snapshot_dir =
            dataset_dir
                .join("snapshots")
                .join(format!("{}-{}", now.date(), now.unix_timestamp()));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let manifest_path = snapshot_dir.join("manifest.json");
        write_json(&manifest_path, &manifest)?;
        write_json(&dataset_dir.join("latest-manifest.json"), &manifest)?;
        write_json(&dataset_dir.join("universe.json"), &universe)?;
        Ok(NasdaqMarketHistoryCollection {
            dataset_dir,
            manifest_path,
            instruments: histories.len(),
            bars: manifest.bars,
            failures: manifest.instruments_failed,
        })
    }

    /// Official Nasdaq Global Index Watch history used for benchmark
    /// attribution. SOD and EOD are fetched separately and joined by exchange
    /// session; provider response decoding remains in this shared data crate.
    pub fn nasdaq_index_history(
        &self,
        symbol: &str,
        start: Date,
        end: Date,
    ) -> Result<BenchmarkHistory, String> {
        if symbol.trim().is_empty() || end <= start {
            return Err("benchmark symbol and an increasing date range are required".into());
        }
        let sod = self.nasdaq_index_rows(symbol, start, end, "SOD")?;
        let eod = self.nasdaq_index_rows(symbol, start, end, "EOD")?;
        let mut starts = BTreeMap::new();
        for row in sod {
            if let Some(value) = valid_positive(row.value) {
                starts.insert(index_date(&row.time_stamp)?, value);
            }
        }
        let mut bars = Vec::new();
        let mut currency = None;
        for row in eod {
            let date = index_date(&row.time_stamp)?;
            let Some(start_value) = starts.get(&date).copied() else {
                continue;
            };
            let Some(end_value) = valid_positive(row.value) else {
                continue;
            };
            currency = currency.or(row.currency);
            bars.push(BenchmarkBar {
                date,
                start_value,
                end_value,
                high_value: valid_positive(row.high),
                low_value: valid_positive(row.low),
            });
        }
        bars.sort_by_key(|bar| bar.date);
        bars.dedup_by_key(|bar| bar.date);
        if bars.len() < 30 {
            return Err(format!(
                "Nasdaq index {symbol} returned only {} joined SOD/EOD sessions",
                bars.len()
            ));
        }
        let (name, return_type) = match symbol {
            "OMXSGI" => ("OMX Stockholm All-Share Gross Index", "gross_total_return"),
            "OMXSPI" => ("OMX Stockholm All-Share Index", "price_return"),
            _ => (symbol, "provider_defined"),
        };
        Ok(BenchmarkHistory {
            format_version: "nasdaq-index-sod-eod-1".into(),
            symbol: symbol.into(),
            name: name.into(),
            return_type: return_type.into(),
            currency: currency.unwrap_or_else(|| "SEK".into()),
            source: format!("{NASDAQ_INDEXES}/Index/History/{symbol}"),
            generated_at: OffsetDateTime::now_utc().to_string(),
            bars,
        })
    }

    fn nasdaq_index_rows(
        &self,
        symbol: &str,
        start: Date,
        end: Date,
        time_of_day: &str,
    ) -> Result<Vec<NasdaqIndexRow>, String> {
        let start = format!("{start}T00:00:00.000");
        let end = format!("{end}T00:00:00.000");
        let response: NasdaqIndexResponse = self
            .client
            .post(format!("{NASDAQ_INDEXES}/Index/HistoryData"))
            .form(&[
                ("id", symbol),
                ("startDate", start.as_str()),
                ("endDate", end.as_str()),
                ("timeOfDay", time_of_day),
            ])
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| format!("Nasdaq index {symbol} {time_of_day}: {error}"))?
            .json()
            .map_err(|error| format!("Nasdaq index {symbol} {time_of_day} response: {error}"))?;
        Ok(response.rows)
    }

    pub fn collect_stockholm(
        &self,
        root: &Path,
        start: Date,
        end: Date,
    ) -> Result<DatasetManifest, String> {
        if end <= start {
            return Err("history end must follow start".into());
        }
        std::fs::create_dir_all(root.join("bars")).map_err(|e| e.to_string())?;
        let universe = self.stockholm_universe()?;
        let results: Vec<_> = universe
            .par_iter()
            .map(|instrument| {
                self.yahoo_history(instrument, start, end)
                    .map(|bars| InstrumentHistory {
                        instrument: instrument.clone(),
                        bars,
                    })
                    .map_err(|error| (instrument.orderbook_id.clone(), error))
            })
            .collect();
        let mut failures = BTreeMap::new();
        let mut histories = Vec::new();
        for result in results {
            match result {
                Ok(history) => histories.push(history),
                Err((id, error)) => {
                    failures.insert(id, error);
                }
            }
        }
        histories.sort_by(|a, b| a.instrument.orderbook_id.cmp(&b.instrument.orderbook_id));
        for history in &histories {
            let path = root
                .join("bars")
                .join(format!("{}.json", history.instrument.orderbook_id));
            write_json(&path, history)?;
        }
        write_json(&root.join("universe.json"), &universe)?;
        let manifest = DatasetManifest {
            format_version: "stockholm-public-current-survivors-1".into(),
            generated_at: OffsetDateTime::now_utc().to_string(),
            requested_start: start,
            requested_end: end,
            universe_source: "Nasdaq Nordic screener current STO segments".into(),
            history_source: "Yahoo Finance chart daily OHLCV/adjusted-close".into(),
            survivorship_status: "SURVIVORSHIP_CONTAMINATED".into(),
            instruments_discovered: universe.len(),
            instruments_with_history: histories.len(),
            instruments_failed: failures.len(),
            bars: histories.iter().map(|history| history.bars.len()).sum(),
            failures,
        };
        write_json(&root.join("manifest.json"), &manifest)?;
        Ok(manifest)
    }

    /// Archive and normalize Finansinspektionen's official public short-
    /// position workbooks. This is a crowding/demand dataset; it must not be
    /// interpreted as historical locate availability or stock-loan supply.
    pub fn collect_fi_net_shorts(&self, root: &Path) -> Result<FiNetShortCollection, String> {
        let historical_bytes = self.download_bytes(FI_SHORT_HISTORICAL)?;
        let aggregate_bytes = self.download_bytes(FI_SHORT_AGGREGATE)?;
        let historical = parse_fi_historical(&historical_bytes)?;
        let aggregate = parse_fi_aggregate(&aggregate_bytes)?;
        if historical.is_empty() || aggregate.is_empty() {
            return Err("FI net-short workbooks parsed to an empty dataset".into());
        }

        let now = OffsetDateTime::now_utc();
        let snapshot_name = format!("{}-{}", now.date(), now.unix_timestamp());
        let snapshot_dir = root
            .join("fi-net-short")
            .join("snapshots")
            .join(snapshot_name);
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let raw_historical = snapshot_dir.join("historical.ods");
        let raw_aggregate = snapshot_dir.join("aggregate.ods");
        std::fs::write(&raw_historical, historical_bytes)
            .map_err(|error| format!("{}: {error}", raw_historical.display()))?;
        std::fs::write(&raw_aggregate, aggregate_bytes)
            .map_err(|error| format!("{}: {error}", raw_aggregate.display()))?;

        let dataset = FiNetShortDataset {
            format_version: "fi-net-short-1".into(),
            generated_at: now.to_string(),
            source_page: FI_SHORT_PAGE.into(),
            historical_source: FI_SHORT_HISTORICAL.into(),
            aggregate_source: FI_SHORT_AGGREGATE.into(),
            raw_historical_file: "historical.ods".into(),
            raw_aggregate_file: "aggregate.ods".into(),
            limitations: vec![
                "Historical holder-level rows are public only at or above 0.5%; threshold-exit rows are censored as <0.5 rather than measured values".into(),
                "Aggregate current positions are published from 0.1%".into(),
                "This is disclosed short demand/crowding, not borrow availability, locate inventory, utilization, or a borrow fee history".into(),
                "Finansinspektionen states that automatically published reports are not reviewed and completeness is not guaranteed".into(),
            ],
            historical,
            aggregate,
        };
        let dataset_path = snapshot_dir.join("net-short.json");
        write_json(&dataset_path, &dataset)?;
        let latest_path = root.join("fi-net-short").join("latest.json");
        write_json(&latest_path, &dataset)?;
        Ok(FiNetShortCollection {
            snapshot_dir,
            dataset_path,
            historical_positions: dataset.historical.len(),
            aggregate_positions: dataset.aggregate.len(),
        })
    }

    /// Archive Skatteverket's equity-history catalogue pages and normalize
    /// their company links. Company event pages cover listings, list changes,
    /// corporate actions, and delistings, but the catalogue spans several
    /// Swedish venues and does not itself provide ISIN-keyed membership.
    pub fn collect_skv_equity_history_catalogue(
        &self,
        root: &Path,
    ) -> Result<SkvEquityHistoryCollection, String> {
        let root_bytes = self.download_bytes(SKV_EQUITY_HISTORY)?;
        let root_html = String::from_utf8(root_bytes.clone())
            .map_err(|error| format!("Skatteverket catalogue encoding: {error}"))?;
        let mut group_urls = skv_links(&root_html, false)?;
        group_urls.insert(SKV_EQUITY_HISTORY.into());

        let now = OffsetDateTime::now_utc();
        let snapshot_name = format!("{}-{}", now.date(), now.unix_timestamp());
        let snapshot_dir = root
            .join("skatteverket-equity-history")
            .join("snapshots")
            .join(snapshot_name);
        let raw_dir = snapshot_dir.join("raw");
        std::fs::create_dir_all(&raw_dir).map_err(|error| error.to_string())?;
        let mut source_pages = Vec::new();
        let mut companies = BTreeMap::new();
        for (index, url) in group_urls.into_iter().enumerate() {
            let bytes = if url == SKV_EQUITY_HISTORY {
                root_bytes.clone()
            } else {
                self.download_bytes(&url)?
            };
            let html = String::from_utf8(bytes.clone())
                .map_err(|error| format!("Skatteverket page {url} encoding: {error}"))?;
            let archive_file = format!("raw/catalogue-{index:02}.html");
            let archive_path = snapshot_dir.join(&archive_file);
            std::fs::write(&archive_path, bytes)
                .map_err(|error| format!("{}: {error}", archive_path.display()))?;
            source_pages.push(SkvEquityHistorySourcePage {
                url: url.clone(),
                archive_file,
            });
            for company in skv_companies(&html)? {
                companies.insert(company.url.clone(), company);
            }
        }
        if companies.len() < 500 {
            return Err(format!(
                "Skatteverket catalogue returned only {} company links",
                companies.len()
            ));
        }
        let dataset = SkvEquityHistoryCatalogue {
            format_version: "skv-equity-history-catalogue-1".into(),
            generated_at: now.to_string(),
            source: SKV_EQUITY_HISTORY.into(),
            source_pages,
            limitations: vec![
                "Catalogue spans Nasdaq Stockholm, First North, NGM, Spotlight, PepMarket, and selected foreign/unlisted companies; venue must be reconstructed from dated company events".into(),
                "Catalogue links are company/name keyed and do not supply an ISIN-keyed point-in-time membership table".into(),
                "Skatteverket states that monitoring stops after delisting and that older events can be incomplete".into(),
                "This catalogue is reference data for a subsequent event-page parser; it is not yet a survivorship-safe tradable universe".into(),
            ],
            companies: companies.into_values().collect(),
        };
        let catalogue_path = snapshot_dir.join("catalogue.json");
        write_json(&catalogue_path, &dataset)?;
        write_json(
            &root.join("skatteverket-equity-history").join("latest.json"),
            &dataset,
        )?;
        Ok(SkvEquityHistoryCollection {
            snapshot_dir,
            catalogue_path,
            companies: dataset.companies.len(),
            source_pages: dataset.source_pages.len(),
        })
    }

    /// Resumably archive and parse the listing-history table from each
    /// Skatteverket company page. Rows retain the authority's original text;
    /// classification is only a discovery hint and cannot substitute for an
    /// ISIN/date mapping before entering a point-in-time universe.
    pub fn collect_skv_listing_history(
        &self,
        root: &Path,
        catalogue: &SkvEquityHistoryCatalogue,
        pause_ms: u64,
        limit: usize,
    ) -> Result<SkvListingHistoryCollection, String> {
        let companies = if limit == 0 {
            &catalogue.companies[..]
        } else {
            &catalogue.companies[..limit.min(catalogue.companies.len())]
        };
        let cache_dir = root.join("skatteverket-equity-history").join("event-pages");
        std::fs::create_dir_all(&cache_dir).map_err(|error| error.to_string())?;
        let mut failures = BTreeMap::new();
        let mut rows = Vec::new();
        let mut archived = 0;
        for (index, company) in companies.iter().enumerate() {
            let cache_path = cache_dir.join(skv_cache_name(&company.url)?);
            let bytes = if cache_path.exists() {
                match std::fs::read(&cache_path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        failures.insert(company.url.clone(), error.to_string());
                        continue;
                    }
                }
            } else {
                match self.download_bytes(&company.url) {
                    Ok(bytes) => {
                        if let Err(error) = std::fs::write(&cache_path, &bytes) {
                            failures.insert(company.url.clone(), error.to_string());
                            continue;
                        }
                        if pause_ms > 0 {
                            std::thread::sleep(Duration::from_millis(pause_ms));
                        }
                        bytes
                    }
                    Err(error) => {
                        failures.insert(company.url.clone(), error);
                        continue;
                    }
                }
            };
            let html = match String::from_utf8(bytes) {
                Ok(html) => html,
                Err(error) => {
                    failures.insert(company.url.clone(), error.to_string());
                    continue;
                }
            };
            match skv_listing_rows(company, &html) {
                Ok(mut parsed) => {
                    archived += 1;
                    rows.append(&mut parsed);
                }
                Err(error) => {
                    failures.insert(company.url.clone(), error);
                }
            }
            if (index + 1) % 100 == 0 {
                eprintln!(
                    "Skatteverket event pages: {}/{} processed, {} failures",
                    index + 1,
                    companies.len(),
                    failures.len()
                );
            }
        }
        rows.sort_by(|a, b| {
            a.company_name
                .cmp(&b.company_name)
                .then_with(|| a.year.cmp(&b.year))
                .then_with(|| a.comment.cmp(&b.comment))
        });
        let now = OffsetDateTime::now_utc();
        let snapshot_name = format!("{}-{}", now.date(), now.unix_timestamp());
        let snapshot_dir = root
            .join("skatteverket-equity-history")
            .join("listing-snapshots")
            .join(snapshot_name);
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset = SkvListingHistoryDataset {
            format_version: "skv-listing-history-2".into(),
            generated_at: now.to_string(),
            source_catalogue: catalogue.source.clone(),
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            pause_ms,
            companies_requested: companies.len(),
            companies_archived: archived,
            failures,
            limitations: vec![
                "Rows are company/name keyed and must be mapped to stable ISIN validity intervals before point-in-time use".into(),
                "Market hints are conservative text classifications, not exchange assertions".into(),
                "The year column and raw Swedish comment are retained; exact effective dates are parsed only when an unambiguous Swedish day/month occurs and still require review".into(),
                "The catalogue spans multiple Swedish venues and selected foreign/unlisted companies".into(),
            ],
            rows,
        };
        let dataset_path = snapshot_dir.join("listing-history.json");
        write_json(&dataset_path, &dataset)?;
        write_json(
            &root
                .join("skatteverket-equity-history")
                .join("latest-listing-history.json"),
            &dataset,
        )?;
        Ok(SkvListingHistoryCollection {
            dataset_path,
            companies_requested: dataset.companies_requested,
            companies_archived: dataset.companies_archived,
            listing_rows: dataset.rows.len(),
            failures: dataset.failures.len(),
        })
    }

    /// Archive FI's public PDMR exports in bounded publication-date slices.
    /// Publication date, not transaction date, defines causal availability.
    /// Raw UTF-16LE CSV files are cached before normalization, making long
    /// backfills resumable and keeping provider decoding out of feature code.
    pub fn collect_fi_pdmr(
        &self,
        root: &Path,
        start: Date,
        end: Date,
        pause_ms: u64,
        interval_days: usize,
    ) -> Result<FiPdmrCollection, String> {
        if end < start {
            return Err("FI PDMR end precedes start".into());
        }
        if !(1..=14).contains(&interval_days) {
            return Err("FI PDMR interval days must be in 1..=14".into());
        }
        let cache_dir = root.join("fi-pdmr").join("raw");
        std::fs::create_dir_all(&cache_dir).map_err(|error| error.to_string())?;
        let mut transactions = BTreeMap::new();
        let mut interval_start = start;
        let mut effective_interval_days = interval_days;
        let mut calm_intervals = 0_usize;
        let mut intervals = 0;
        let mut next_progress = 25;
        let mut fresh_requests_in_burst = 0_usize;
        while interval_start <= end {
            let interval_end = (interval_start
                + time::Duration::days(effective_interval_days.saturating_sub(1) as i64))
            .min(end);
            let (parsed, exports, split) = self.collect_fi_pdmr_interval(
                &cache_dir,
                interval_start,
                interval_end,
                pause_ms,
                &mut fresh_requests_in_burst,
            )?;
            if split {
                calm_intervals = 0;
                if effective_interval_days > 1 {
                    effective_interval_days = effective_interval_days.div_ceil(2);
                    eprintln!(
                        "FI PDMR: reducing subsequent intervals to {effective_interval_days} days"
                    );
                }
            } else if effective_interval_days < interval_days && parsed.len() < 600 {
                calm_intervals += 1;
                if calm_intervals == 8 {
                    effective_interval_days =
                        (effective_interval_days * 3).div_ceil(2).min(interval_days);
                    calm_intervals = 0;
                    eprintln!(
                        "FI PDMR: widening subsequent intervals to {effective_interval_days} days"
                    );
                }
            } else {
                calm_intervals = 0;
            }
            for transaction in parsed {
                transactions.insert(fi_pdmr_key(&transaction), transaction);
            }
            intervals += exports;
            if intervals >= next_progress {
                eprintln!(
                    "FI PDMR: {intervals} exports through {interval_end}, {} unique rows",
                    transactions.len()
                );
                next_progress = (intervals / 25 + 1) * 25;
            }
            interval_start = interval_end.next_day().ok_or("FI PDMR date overflow")?;
        }
        let now = OffsetDateTime::now_utc();
        let transactions = transactions.into_values().collect::<Vec<_>>();
        let unique_isins = transactions
            .iter()
            .filter_map(|transaction| transaction.isin.as_deref())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let dataset = FiPdmrDataset {
            format_version: "fi-pdmr-publication-sliced-1".into(),
            generated_at: now.to_string(),
            source_page: FI_PDMR_PAGE.into(),
            export_endpoint: FI_PDMR_EXPORT.into(),
            requested_start: start,
            requested_end: end,
            query_basis: "publication_date".into(),
            interval_days,
            pause_ms,
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            limitations: vec![
                "A transaction becomes usable only on publication_date, never transaction_date".into(),
                "FI publishes reports automatically without prior review and does not guarantee completeness or correctness".into(),
                "Reporting thresholds mean some transactions are legally unreported".into(),
                "interval_days is a maximum; dense publication ranges are recursively split below FI's export ceiling".into(),
                "Amendments and current status are preserved; causal consumers must not use today's status to rewrite earlier availability".into(),
                "The register covers regulated markets and MTFs; the Stockholm Main Market universe must be joined by point-in-time ISIN".into(),
            ],
            transactions,
        };
        let snapshot_dir = root.join("fi-pdmr").join("snapshots").join(format!(
            "{}-{}",
            now.date(),
            now.unix_timestamp()
        ));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset_path = snapshot_dir.join("pdmr.json");
        write_json(&dataset_path, &dataset)?;
        write_json(&root.join("fi-pdmr").join("latest.json"), &dataset)?;
        Ok(FiPdmrCollection {
            dataset_path,
            transactions: dataset.transactions.len(),
            unique_isins,
            intervals,
        })
    }

    fn collect_fi_pdmr_interval(
        &self,
        cache_dir: &Path,
        interval_start: Date,
        interval_end: Date,
        pause_ms: u64,
        fresh_requests_in_burst: &mut usize,
    ) -> Result<(Vec<FiPdmrTransaction>, usize, bool), String> {
        let cache_path = cache_dir.join(format!(
            "publication-{}_{}.csv",
            interval_start, interval_end
        ));
        let bytes = if cache_path.exists() {
            std::fs::read(&cache_path)
                .map_err(|error| format!("{}: {error}", cache_path.display()))?
        } else {
            if *fresh_requests_in_burst == 7 {
                eprintln!("FI PDMR: respecting export burst cooldown (60s)");
                std::thread::sleep(Duration::from_secs(60));
                *fresh_requests_in_burst = 0;
            }
            let from = fi_query_date(interval_start);
            let to = fi_query_date(interval_end);
            let bytes = self.download_fi_pdmr_export(&from, &to, interval_start, interval_end)?;
            std::fs::write(&cache_path, &bytes)
                .map_err(|error| format!("{}: {error}", cache_path.display()))?;
            if pause_ms > 0 {
                std::thread::sleep(Duration::from_millis(pause_ms));
            }
            *fresh_requests_in_burst += 1;
            bytes
        };
        let parsed = parse_fi_pdmr_csv(&bytes)
            .map_err(|error| format!("FI PDMR {interval_start}..{interval_end}: {error}"))?;
        if parsed.len() < 950 {
            return Ok((parsed, 1, false));
        }
        if interval_start == interval_end {
            return Err(format!(
                "FI PDMR {interval_start} returned {} rows near the 1000-row export ceiling and cannot be split further",
                parsed.len()
            ));
        }
        let span = (interval_end - interval_start).whole_days();
        let left_end = interval_start + time::Duration::days(span / 2);
        let right_start = left_end.next_day().ok_or("FI PDMR date overflow")?;
        eprintln!("FI PDMR: splitting {interval_start}..{interval_end} at the export ceiling");
        let (mut left, left_exports, _) = self.collect_fi_pdmr_interval(
            cache_dir,
            interval_start,
            left_end,
            pause_ms,
            fresh_requests_in_burst,
        )?;
        let (right, right_exports, _) = self.collect_fi_pdmr_interval(
            cache_dir,
            right_start,
            interval_end,
            pause_ms,
            fresh_requests_in_burst,
        )?;
        left.extend(right);
        Ok((left, left_exports + right_exports, true))
    }

    fn download_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        self.client
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| format!("{url}: {error}"))?
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|error| format!("{url}: {error}"))
    }

    fn download_fi_pdmr_export(
        &self,
        from: &str,
        to: &str,
        interval_start: Date,
        interval_end: Date,
    ) -> Result<Vec<u8>, String> {
        let mut last_error = String::new();
        for attempt in 1..=4_u64 {
            // FI's legacy ASP.NET export endpoint eventually rejects a
            // long-lived rustls client even when the prior response requested
            // close. Isolate each bounded export in a fresh client; pacing is
            // still enforced by the outer collector.
            let client = reqwest::blocking::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|error| error.to_string())?;
            let result = client
                .get(FI_PDMR_EXPORT)
                .header(reqwest::header::CONNECTION, "close")
                .query(&[
                    ("SearchFunctionType", "Insyn"),
                    ("Publiceringsdatum.From", from),
                    ("Publiceringsdatum.To", to),
                    ("button", "export"),
                ])
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .and_then(|response| response.bytes());
            match result {
                Ok(bytes) => return Ok(bytes.to_vec()),
                Err(error) => {
                    last_error = error.to_string();
                    if attempt < 4 {
                        std::thread::sleep(Duration::from_secs(attempt * 20));
                    }
                }
            }
        }
        Err(format!(
            "FI PDMR {interval_start}..{interval_end} failed after 4 attempts: {last_error}"
        ))
    }

    /// Archive official Nasdaq Stockholm Main Market financial-report
    /// announcements. This records public event timestamps and categories; it
    /// deliberately does not scrape PDF values or infer accounting revisions.
    pub fn collect_nasdaq_financial_reports(
        &self,
        root: &Path,
        start: Date,
        end: Date,
        pause_ms: u64,
    ) -> Result<NasdaqCompanyNewsCollection, String> {
        if end < start {
            return Err("Nasdaq company-news end precedes start".into());
        }
        let cache_dir = root.join("nasdaq-company-news").join("raw");
        std::fs::create_dir_all(&cache_dir).map_err(|error| error.to_string())?;
        let mut announcements = BTreeMap::new();
        let mut requests = 0_usize;
        for (category_index, category) in NASDAQ_FINANCIAL_REPORT_CATEGORIES.iter().enumerate() {
            let (items, pages) = self.collect_nasdaq_news_interval(
                &cache_dir,
                category_index,
                category,
                start,
                end,
                pause_ms,
            )?;
            requests += pages;
            for item in items {
                announcements.insert(item.disclosure_id, item);
            }
            eprintln!(
                "Nasdaq company news: {category:?} complete, {} unique announcements",
                announcements.len()
            );
        }
        let announcements = announcements.into_values().collect::<Vec<_>>();
        let companies = announcements
            .iter()
            .map(|announcement| announcement.company.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let now = OffsetDateTime::now_utc();
        let dataset = NasdaqCompanyNewsDataset {
            format_version: "nasdaq-stockholm-financial-report-news-1".into(),
            generated_at: now.to_string(),
            source_page: NASDAQ_COMPANY_NEWS_PAGE.into(),
            query_endpoint: NASDAQ_COMPANY_NEWS_API.into(),
            requested_start: start,
            requested_end: end,
            market: "Main Market, Stockholm".into(),
            categories: NASDAQ_FINANCIAL_REPORT_CATEGORIES
                .iter()
                .map(|category| (*category).into())
                .collect(),
            pause_ms,
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            limitations: vec![
                "Announcements are issuer-name keyed because the public response does not expose an ISIN; consumers must publish name-mapping coverage and ambiguity diagnostics".into(),
                "A disclosure becomes usable only after its published timestamp; daily research conservatively admits it on later decision dates".into(),
                "Categories identify a financial-report release but do not provide standardized point-in-time accounting values or revision histories".into(),
                "Multiple languages and corrected disclosures are retained by disclosure ID; causal feature code must avoid treating translations as independent economic events".into(),
                "The archive is restricted to Nasdaq Main Market, Stockholm and the declared report categories; First North and general company news are excluded".into(),
            ],
            announcements,
        };
        let snapshot_dir = root
            .join("nasdaq-company-news")
            .join("snapshots")
            .join(format!("{}-{}", now.date(), now.unix_timestamp()));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset_path = snapshot_dir.join("financial-reports.json");
        write_json(&dataset_path, &dataset)?;
        write_json(
            &root
                .join("nasdaq-company-news")
                .join("latest-financial-reports.json"),
            &dataset,
        )?;
        Ok(NasdaqCompanyNewsCollection {
            dataset_path,
            announcements: dataset.announcements.len(),
            companies,
            requests,
        })
    }

    /// Resumably archive the official HTML body and attachment metadata for
    /// Nasdaq Stockholm financial-report announcements. Provider transport
    /// and HTML decoding stay here; consumers receive owned text records and
    /// must version any accounting or language interpretation separately.
    #[allow(clippy::too_many_arguments)]
    pub fn collect_nasdaq_financial_report_messages(
        &self,
        root: &Path,
        metadata_source: &Path,
        metadata: &NasdaqCompanyNewsDataset,
        pause_ms: u64,
        concurrency: usize,
        limit: usize,
    ) -> Result<NasdaqFinancialReportMessageCollection, String> {
        if concurrency == 0 {
            return Err("Nasdaq report-message concurrency must be positive".into());
        }
        let cache_dir = root.join("nasdaq-financial-report-messages").join("raw");
        let message_dir = cache_dir.join("messages");
        std::fs::create_dir_all(&message_dir).map_err(|error| error.to_string())?;
        let mut announcements = metadata
            .announcements
            .iter()
            .filter(|announcement| {
                NASDAQ_FINANCIAL_REPORT_CATEGORIES.contains(&announcement.category.as_str())
            })
            .collect::<Vec<_>>();
        announcements.sort_by_key(|announcement| announcement.disclosure_id);
        announcements.dedup_by_key(|announcement| announcement.disclosure_id);
        if limit > 0 {
            announcements.truncate(limit);
        }
        if announcements.is_empty() {
            return Err("Nasdaq report-message metadata has no financial reports".into());
        }

        let completed = AtomicUsize::new(0);
        let requested = announcements.len();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(concurrency)
            .build()
            .map_err(|error| error.to_string())?;
        let results = pool.install(|| {
            announcements
                .par_iter()
                .map(|announcement| {
                    let path = message_dir.join(format!("{}.html", announcement.disclosure_id));
                    let result = (|| {
                        let bytes = if path.exists() {
                            std::fs::read(&path)
                                .map_err(|error| format!("{}: {error}", path.display()))?
                        } else {
                            let bytes =
                                self.download_bytes_with_retries(&announcement.message_url, 4, 2)?;
                            std::fs::write(&path, &bytes)
                                .map_err(|error| format!("{}: {error}", path.display()))?;
                            if pause_ms > 0 {
                                std::thread::sleep(Duration::from_millis(pause_ms));
                            }
                            bytes
                        };
                        parse_nasdaq_financial_report_message(announcement, &bytes, &path)
                    })();
                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    if done % 250 == 0 || done == requested {
                        eprintln!("Nasdaq report messages: {done}/{requested} processed");
                    }
                    (announcement.disclosure_id, result)
                })
                .collect::<Vec<_>>()
        });
        let mut failures = BTreeMap::new();
        let mut messages = Vec::new();
        for (disclosure_id, result) in results {
            match result {
                Ok(message) => messages.push(message),
                Err(error) => {
                    failures.insert(disclosure_id, error);
                }
            }
        }
        messages.sort_by_key(|message| {
            (
                message.announcement.publication_date,
                message.announcement.disclosure_id,
            )
        });
        let attachments = messages
            .iter()
            .map(|message| message.attachments.len())
            .sum();
        let now = OffsetDateTime::now_utc();
        let dataset = NasdaqFinancialReportMessageDataset {
            format_version: "nasdaq-stockholm-financial-report-messages-1".into(),
            generated_at: now.to_string(),
            source_page: NASDAQ_COMPANY_NEWS_PAGE.into(),
            metadata_source: metadata_source.to_string_lossy().into_owned(),
            pause_ms,
            concurrency,
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            requested_messages: requested,
            limitations: vec![
                "The archive preserves official issuer-authored text and attachment links; it does not treat unstructured wording as standardized audited accounting data".into(),
                "HTML bodies vary by issuer, language, period, correction, and publication agent. Downstream extraction must retain missingness and publish field-level coverage".into(),
                "Attachment URLs are metadata only in this version; PDFs are not downloaded or parsed".into(),
                "Issuer-name mapping, translation/correction deduplication, and causal availability remain consumer responsibilities".into(),
            ],
            message_failures: failures,
            messages,
        };
        let snapshot_dir = root
            .join("nasdaq-financial-report-messages")
            .join("snapshots")
            .join(format!("{}-{}", now.date(), now.unix_timestamp()));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset_path = snapshot_dir.join("financial-report-messages.json");
        write_json(&dataset_path, &dataset)?;
        write_json(
            &root
                .join("nasdaq-financial-report-messages")
                .join("latest-financial-report-messages.json"),
            &dataset,
        )?;
        Ok(NasdaqFinancialReportMessageCollection {
            dataset_path,
            requested,
            messages: dataset.messages.len(),
            attachments,
            failures: dataset.message_failures.len(),
        })
    }

    /// Resumably archive and extract text from the PDF attachments referenced
    /// by the official financial-report messages. Transport and PDF decoding
    /// remain in the shared data-source crate; accounting interpretation stays
    /// in the versioned Rust feature crate.
    #[allow(clippy::too_many_arguments)]
    pub fn collect_nasdaq_financial_report_attachments(
        &self,
        root: &Path,
        message_metadata_source: &Path,
        metadata: &NasdaqFinancialReportMessageDataset,
        pause_ms: u64,
        concurrency: usize,
        max_attachment_bytes: u64,
        limit: usize,
        cached_only: bool,
    ) -> Result<NasdaqFinancialReportAttachmentCollection, String> {
        if concurrency == 0 {
            return Err("Nasdaq report-attachment concurrency must be positive".into());
        }
        if max_attachment_bytes == 0 {
            return Err("Nasdaq report-attachment byte ceiling must be positive".into());
        }
        let cache_dir = root.join("nasdaq-financial-report-attachments").join("raw");
        let text_dir = root
            .join("nasdaq-financial-report-attachments")
            .join("text");
        std::fs::create_dir_all(&cache_dir).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&text_dir).map_err(|error| error.to_string())?;

        let mut sources = BTreeMap::<String, (BTreeSet<String>, BTreeSet<u64>)>::new();
        for message in &metadata.messages {
            for attachment in &message.attachments {
                if !attachment.name.to_ascii_lowercase().ends_with(".pdf") {
                    continue;
                }
                let entry = sources.entry(attachment.url.clone()).or_default();
                entry.0.insert(attachment.name.clone());
                entry.1.insert(message.announcement.disclosure_id);
            }
        }
        let available = sources.len();
        let mut sources = sources.into_iter().collect::<Vec<_>>();
        if limit > 0 {
            sources.truncate(limit);
        }
        let requested = sources.len();
        if sources.is_empty() {
            return Err("Nasdaq report-message metadata has no PDF attachments".into());
        }

        let completed = AtomicUsize::new(0);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(concurrency)
            .build()
            .map_err(|error| error.to_string())?;
        let mut documents = pool.install(|| {
            sources
                .par_iter()
                .map(|(url, (names, disclosure_ids))| {
                    let key = nasdaq_attachment_cache_key(url);
                    let raw_path = cache_dir.join(format!("{key}.pdf"));
                    let text_path = text_dir.join(format!("{key}.txt"));
                    let mut document = NasdaqFinancialReportAttachmentDocument {
                        url: url.clone(),
                        names: names.iter().cloned().collect(),
                        disclosure_ids: disclosure_ids.iter().copied().collect(),
                        byte_length: None,
                        raw_file: None,
                        extracted_text_file: None,
                        extracted_text_chars: 0,
                        failure: None,
                    };
                    let result = (|| {
                        let cached = raw_path
                            .exists()
                            .then(|| read_bounded_file(&raw_path, max_attachment_bytes))
                            .transpose()?;
                        let bytes = if cached
                            .as_ref()
                            .is_some_and(|bytes| validate_pdf_bytes(bytes).is_ok())
                        {
                            cached.expect("validated cached bytes exist")
                        } else if cached_only {
                            if let Some(bytes) = cached {
                                validate_pdf_bytes(&bytes)?;
                            }
                            return Err(
                                "PDF is not present in a valid cache; network is disabled".into()
                            );
                        } else {
                            let bytes = self.download_bounded_bytes_with_retries(
                                url,
                                max_attachment_bytes,
                                4,
                                2,
                            )?;
                            validate_pdf_bytes(&bytes)?;
                            write_atomic_bytes(&raw_path, &bytes)?;
                            if pause_ms > 0 {
                                std::thread::sleep(Duration::from_millis(pause_ms));
                            }
                            bytes
                        };
                        validate_pdf_bytes(&bytes)?;
                        document.byte_length = Some(bytes.len() as u64);
                        document.raw_file = Some(raw_path.to_string_lossy().into_owned());
                        if text_path.exists() {
                            let text = std::fs::read_to_string(&text_path)
                                .map_err(|error| format!("{}: {error}", text_path.display()))?;
                            document.extracted_text_chars = text.chars().count();
                            if document.extracted_text_chars == 0 {
                                return Err("cached PDF text is empty".into());
                            }
                            document.extracted_text_file =
                                Some(text_path.to_string_lossy().into_owned());
                        }
                        Ok::<(), String>(())
                    })();
                    if let Err(error) = result {
                        document.failure = Some(error);
                    }
                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    if done % 250 == 0 || done == requested {
                        eprintln!("Nasdaq report attachments: {done}/{requested} processed");
                    }
                    document
                })
                .collect::<Vec<_>>()
        });
        documents.sort_by(|a, b| a.url.cmp(&b.url));
        let to_extract = documents
            .iter()
            .filter(|document| {
                document.raw_file.is_some()
                    && document.extracted_text_file.is_none()
                    && document.failure.is_none()
            })
            .count();
        let mut extracted = 0_usize;
        for document in &mut documents {
            let (Some(raw_file), None) = (
                document.raw_file.as_deref(),
                document.extracted_text_file.as_deref(),
            ) else {
                continue;
            };
            if document.failure.is_some() {
                continue;
            }
            let text_path = text_dir.join(format!(
                "{}.txt",
                nasdaq_attachment_cache_key(&document.url)
            ));
            match run_isolated_pdf_extractor(Path::new(raw_file), &text_path, max_attachment_bytes)
                .and_then(|()| {
                    std::fs::read_to_string(&text_path)
                        .map_err(|error| format!("{}: {error}", text_path.display()))
                }) {
                Ok(text) if !text.is_empty() => {
                    document.extracted_text_chars = text.chars().count();
                    document.extracted_text_file = Some(text_path.to_string_lossy().into_owned());
                }
                Ok(_) => document.failure = Some("PDF contains no extractable text".into()),
                Err(error) => document.failure = Some(error),
            }
            extracted += 1;
            if extracted % 50 == 0 || extracted == to_extract {
                eprintln!(
                    "Nasdaq report attachment extraction: {extracted}/{to_extract} isolated workers completed"
                );
            }
        }
        let now = OffsetDateTime::now_utc();
        let dataset = NasdaqFinancialReportAttachmentDataset {
            format_version: "nasdaq-stockholm-financial-report-attachments-1".into(),
            generated_at: now.to_string(),
            source: "Official Nasdaq issuer-news attachment service".into(),
            message_metadata_source: message_metadata_source
                .to_string_lossy()
                .into_owned(),
            pause_ms,
            concurrency,
            max_attachment_bytes,
            network_downloads_enabled: !cached_only,
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            text_cache_dir: text_dir.to_string_lossy().into_owned(),
            available_pdf_urls: available,
            requested_pdf_urls: requested,
            limitations: vec![
                "PDF text extraction preserves issuer wording but not table geometry; accounting values require conservative field-level parsing and missingness".into(),
                "Image-only documents are retained as explicit extraction failures; OCR is not silently substituted".into(),
                format!("Every PDF decoder runs in its own sequential subprocess with a {} MiB address-space limit and a {} second wall-clock timeout", PDF_EXTRACTOR_ADDRESS_SPACE_BYTES / 1024 / 1024, PDF_EXTRACTOR_TIMEOUT_SECONDS),
                "Attachment publication time is inherited from its official message; translation/correction deduplication remains a causal feature responsibility".into(),
                "Only PDF attachments are collected in this version; spreadsheets and XBRL attachments remain metadata".into(),
            ],
            documents,
        };
        let snapshot_dir = root
            .join("nasdaq-financial-report-attachments")
            .join("snapshots")
            .join(format!("{}-{}", now.date(), now.unix_timestamp()));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset_path = snapshot_dir.join("financial-report-attachments.json");
        write_json(&dataset_path, &dataset)?;
        write_json(
            &root
                .join("nasdaq-financial-report-attachments")
                .join("latest-financial-report-attachments.json"),
            &dataset,
        )?;
        Ok(NasdaqFinancialReportAttachmentCollection {
            dataset_path,
            available,
            requested,
            downloaded: dataset
                .documents
                .iter()
                .filter(|document| document.raw_file.is_some())
                .count(),
            extracted: dataset
                .documents
                .iter()
                .filter(|document| document.extracted_text_file.is_some())
                .count(),
            bytes: dataset
                .documents
                .iter()
                .filter_map(|document| document.byte_length)
                .sum(),
            text_chars: dataset
                .documents
                .iter()
                .map(|document| document.extracted_text_chars)
                .sum(),
            failures: dataset
                .documents
                .iter()
                .filter(|document| document.failure.is_some())
                .count(),
        })
    }

    /// Archive the complete official Nasdaq Stockholm Main Market company-news
    /// feed. Unlike [`Self::collect_nasdaq_financial_reports`], this does not
    /// pre-filter categories: downstream feature code can declare a stable
    /// event taxonomy without losing the original provider category/headline.
    pub fn collect_nasdaq_stockholm_company_news(
        &self,
        root: &Path,
        start: Date,
        end: Date,
        pause_ms: u64,
    ) -> Result<NasdaqCompanyNewsCollection, String> {
        if end < start {
            return Err("Nasdaq company-news end precedes start".into());
        }
        let cache_dir = root.join("nasdaq-company-news-all").join("raw");
        std::fs::create_dir_all(&cache_dir).map_err(|error| error.to_string())?;
        let (items, requests) =
            self.collect_all_nasdaq_news_interval(&cache_dir, start, end, pause_ms)?;
        let mut announcements = BTreeMap::new();
        for item in items {
            announcements.insert(item.disclosure_id, item);
        }
        let announcements = announcements.into_values().collect::<Vec<_>>();
        let companies = announcements
            .iter()
            .map(|announcement| announcement.company.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let categories = announcements
            .iter()
            .map(|announcement| announcement.category.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let now = OffsetDateTime::now_utc();
        let dataset = NasdaqCompanyNewsDataset {
            format_version: "nasdaq-stockholm-company-news-all-1".into(),
            generated_at: now.to_string(),
            source_page: NASDAQ_COMPANY_NEWS_PAGE.into(),
            query_endpoint: NASDAQ_COMPANY_NEWS_API.into(),
            requested_start: start,
            requested_end: end,
            market: "Main Market, Stockholm".into(),
            categories,
            pause_ms,
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            limitations: vec![
                "Announcements are issuer-name keyed because the public response does not expose an ISIN; consumers must publish name-mapping coverage and ambiguity diagnostics".into(),
                "A disclosure becomes usable only after its published timestamp; daily research conservatively admits it on later decision dates".into(),
                "Provider categories and headlines are archived verbatim; downstream semantic grouping must be versioned and implemented outside the provider adapter".into(),
                "Multiple languages and corrected disclosures are retained by disclosure ID; causal feature code must avoid treating translations as independent economic events".into(),
                "The archive is restricted to Nasdaq Main Market, Stockholm; First North and other Nordic venues are excluded".into(),
            ],
            announcements,
        };
        let snapshot_dir = root
            .join("nasdaq-company-news-all")
            .join("snapshots")
            .join(format!("{}-{}", now.date(), now.unix_timestamp()));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset_path = snapshot_dir.join("company-news.json");
        write_json(&dataset_path, &dataset)?;
        write_json(
            &root
                .join("nasdaq-company-news-all")
                .join("latest-company-news.json"),
            &dataset,
        )?;
        Ok(NasdaqCompanyNewsCollection {
            dataset_path,
            announcements: dataset.announcements.len(),
            companies,
            requests,
        })
    }

    /// Archive official Nasdaq equity-market notices that can reconstruct
    /// Stockholm listings, delistings, identifier changes, segment changes,
    /// suspensions, and resumptions. The result is discovery/reference data:
    /// it still needs effective-dated ordinary-share classification and price
    /// coverage before it can declare a survivorship-safe universe.
    pub fn collect_nasdaq_stockholm_equity_notices(
        &self,
        root: &Path,
        start: Date,
        end: Date,
        pause_ms: u64,
    ) -> Result<NasdaqEquityNoticeCollection, String> {
        const PAGE_SIZE: usize = 200;
        const CATEGORY: &str = "Equity Market information";
        if end < start {
            return Err("Nasdaq equity-notice end precedes start".into());
        }
        let cache_dir = root.join("nasdaq-equity-notices").join("raw");
        let query_dir = cache_dir.join("query");
        let message_dir = cache_dir.join("messages");
        std::fs::create_dir_all(&query_dir).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&message_dir).map_err(|error| error.to_string())?;

        let first_bytes =
            self.nasdaq_equity_notice_page(&query_dir, start, end, 0, PAGE_SIZE, pause_ms)?;
        let first = parse_nasdaq_news_jsonp(&first_bytes)?;
        if first.count >= 10_000 {
            return Err(format!(
                "Nasdaq equity notices {start}..{end} reached the 10,000-result ceiling; collect shorter intervals"
            ));
        }
        let pages = first.count.div_ceil(PAGE_SIZE).max(1);
        let metadata_notices_seen = first.count;
        let mut responses = vec![first];
        for page in 1..pages {
            let offset = page * PAGE_SIZE;
            let bytes = self
                .nasdaq_equity_notice_page(&query_dir, start, end, offset, PAGE_SIZE, pause_ms)?;
            let response = parse_nasdaq_news_jsonp(&bytes)?;
            if response.results.item.is_empty() {
                return Err(format!(
                    "Nasdaq equity notices returned an empty page at offset {offset}"
                ));
            }
            responses.push(response);
        }

        let mut metadata = responses
            .into_iter()
            .flat_map(|response| response.results.item)
            .filter(|item| item.cns_category == CATEGORY)
            .filter(|item| {
                item.company
                    .to_ascii_lowercase()
                    .contains("nasdaq stockholm")
            })
            .filter_map(|item| {
                let publication_date = parse_nasdaq_publication_date(&item.published).ok()?;
                (publication_date >= start && publication_date <= end)
                    .then_some((item, publication_date))
            })
            .filter(|(item, _)| {
                classify_nasdaq_equity_notice(&item.headline) != NasdaqEquityNoticeKind::Other
            })
            .collect::<Vec<_>>();
        metadata.sort_by_key(|(item, _)| item.disclosure_id);
        metadata.dedup_by_key(|(item, _)| item.disclosure_id);
        let candidate_messages = metadata.len();
        let mut failures = BTreeMap::new();
        let mut notices = Vec::new();
        for (index, (item, publication_date)) in metadata.into_iter().enumerate() {
            let path = message_dir.join(format!("{}.html", item.disclosure_id));
            let bytes = if path.exists() {
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?
            } else {
                match self.download_bytes_with_retries(&item.message_url, 4, 2) {
                    Ok(bytes) => {
                        std::fs::write(&path, &bytes)
                            .map_err(|error| format!("{}: {error}", path.display()))?;
                        if pause_ms > 0 {
                            std::thread::sleep(Duration::from_millis(pause_ms));
                        }
                        bytes
                    }
                    Err(error) => {
                        failures.insert(item.disclosure_id, error);
                        continue;
                    }
                }
            };
            match parse_nasdaq_equity_notice(&item, publication_date, &bytes, &path) {
                Ok(notice)
                    if notice.body_mentions_stockholm
                        || item.headline.to_ascii_lowercase().contains("stockholm") =>
                {
                    notices.push(notice);
                }
                Ok(_) => {}
                Err(error) => {
                    failures.insert(item.disclosure_id, error);
                }
            }
            if (index + 1) % 100 == 0 {
                eprintln!(
                    "Nasdaq equity notices: {}/{} candidate messages, {} Stockholm notices, {} failures",
                    index + 1,
                    candidate_messages,
                    notices.len(),
                    failures.len()
                );
            }
        }
        notices.sort_by_key(|notice| (notice.publication_date, notice.disclosure_id));
        let identifiers = notices
            .iter()
            .filter(|notice| {
                !notice.isins.is_empty()
                    || !notice.short_names.is_empty()
                    || !notice.orderbook_ids.is_empty()
            })
            .count();
        let now = OffsetDateTime::now_utc();
        let dataset = NasdaqEquityNoticeDataset {
            format_version: "nasdaq-stockholm-equity-notices-1".into(),
            generated_at: now.to_string(),
            source_page: NASDAQ_MARKET_NOTICES_PAGE.into(),
            query_endpoint: NASDAQ_COMPANY_NEWS_API.into(),
            requested_start: start,
            requested_end: end,
            category: CATEGORY.into(),
            pause_ms,
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            limitations: vec![
                "Headline classification is a conservative discovery filter; each retained raw official notice remains the authority record".into(),
                "Equity Market information also includes funds, ETPs, rights, and other non-ordinary-share instruments; consumers must intersect identifiers with a validated ordinary-share security master".into(),
                "First/last trading dates are parsed only from explicit English notice phrases or table labels and remain absent when no unambiguous date is present".into(),
                "Notices establish effective events and identifiers, not historical OHLCV, terminal consideration, size bucket, or daily tradability".into(),
            ],
            metadata_notices_seen,
            candidate_messages,
            message_failures: failures,
            notices,
        };
        let snapshot_dir = root
            .join("nasdaq-equity-notices")
            .join("snapshots")
            .join(format!("{}-{}", now.date(), now.unix_timestamp()));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset_path = snapshot_dir.join("stockholm-equity-notices.json");
        write_json(&dataset_path, &dataset)?;
        write_json(
            &root
                .join("nasdaq-equity-notices")
                .join("latest-stockholm-equity-notices.json"),
            &dataset,
        )?;
        Ok(NasdaqEquityNoticeCollection {
            dataset_path,
            metadata_notices_seen,
            candidate_messages,
            notices: dataset.notices.len(),
            identifiers,
            failures: dataset.message_failures.len(),
            requests: pages,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn nasdaq_equity_notice_page(
        &self,
        cache_dir: &Path,
        start: Date,
        end: Date,
        offset: usize,
        limit: usize,
        pause_ms: u64,
    ) -> Result<Vec<u8>, String> {
        let path = cache_dir.join(format!(
            "equity-market-information-{start}_{end}-offset-{offset:06}.jsonp"
        ));
        if path.exists() {
            return std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()));
        }
        let from_boundary = start
            .previous_day()
            .ok_or("Nasdaq equity-notice start-date underflow")?;
        let from = news_boundary_milliseconds(from_boundary)?;
        let to = news_boundary_milliseconds(end)?;
        let mut last_error = String::new();
        for attempt in 1..=4_u64 {
            let result = self
                .client
                .get(NASDAQ_COMPANY_NEWS_API)
                .query(&[
                    ("callback", "handleResponse".to_string()),
                    ("countResults", "true".to_string()),
                    ("globalGroup", "exchangeNotice".to_string()),
                    ("displayLanguage", "en".to_string()),
                    ("timeZone", "CET".to_string()),
                    ("dateMask", "yyyy-MM-dd HH:mm:ss".to_string()),
                    ("limit", limit.to_string()),
                    ("start", offset.to_string()),
                    ("dir", "DESC".to_string()),
                    ("globalName", "ExchangenewsFilter".to_string()),
                    ("cnsCategory", "Equity Market information".to_string()),
                    ("fromDate", from.to_string()),
                    ("toDate", to.to_string()),
                ])
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .and_then(|response| response.bytes());
            match result {
                Ok(bytes) => {
                    let bytes = bytes.to_vec();
                    parse_nasdaq_news_jsonp(&bytes).map_err(|error| {
                        format!("Nasdaq equity-notice response validation failed: {error}")
                    })?;
                    std::fs::write(&path, &bytes)
                        .map_err(|error| format!("{}: {error}", path.display()))?;
                    if pause_ms > 0 {
                        std::thread::sleep(Duration::from_millis(pause_ms));
                    }
                    return Ok(bytes);
                }
                Err(error) => {
                    last_error = error.to_string();
                    if attempt < 4 {
                        std::thread::sleep(Duration::from_secs(attempt * 2));
                    }
                }
            }
        }
        Err(format!(
            "Nasdaq equity notices {start}..{end} offset {offset} failed after 4 attempts: {last_error}"
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_nasdaq_news_interval(
        &self,
        cache_dir: &Path,
        category_index: usize,
        category: &str,
        start: Date,
        end: Date,
        pause_ms: u64,
    ) -> Result<(Vec<NasdaqCompanyAnnouncement>, usize), String> {
        const PAGE_SIZE: usize = 200;
        let first_bytes = self.nasdaq_news_page(
            cache_dir,
            category_index,
            category,
            start,
            end,
            0,
            PAGE_SIZE,
            pause_ms,
        )?;
        let first = parse_nasdaq_news_jsonp(&first_bytes)?;
        if first.count >= 10_000 {
            if start == end {
                return Err(format!(
                    "Nasdaq company news {category:?} on {start} reached the 10,000-result count ceiling"
                ));
            }
            let span = (end - start).whole_days();
            let left_end = start + time::Duration::days(span / 2);
            let right_start = left_end
                .next_day()
                .ok_or("Nasdaq company-news date overflow")?;
            eprintln!(
                "Nasdaq company news: splitting {category:?} {start}..{end} at count ceiling"
            );
            let (mut left, left_pages) = self.collect_nasdaq_news_interval(
                cache_dir,
                category_index,
                category,
                start,
                left_end,
                pause_ms,
            )?;
            let (right, right_pages) = self.collect_nasdaq_news_interval(
                cache_dir,
                category_index,
                category,
                right_start,
                end,
                pause_ms,
            )?;
            left.extend(right);
            return Ok((left, left_pages + right_pages));
        }
        let expected_pages = first.count.div_ceil(PAGE_SIZE);
        let mut responses = vec![first];
        for page in 1..expected_pages {
            let offset = page * PAGE_SIZE;
            let bytes = self.nasdaq_news_page(
                cache_dir,
                category_index,
                category,
                start,
                end,
                offset,
                PAGE_SIZE,
                pause_ms,
            )?;
            let response = parse_nasdaq_news_jsonp(&bytes)?;
            if response.results.item.is_empty() {
                return Err(format!(
                    "Nasdaq company news {category:?} {start}..{end} returned an empty page at offset {offset}"
                ));
            }
            responses.push(response);
        }
        let mut announcements = Vec::new();
        for item in responses
            .into_iter()
            .flat_map(|response| response.results.item)
        {
            if item.cns_category != category {
                return Err(format!(
                    "Nasdaq company-news category filter mismatch: {:?}",
                    item.cns_category
                ));
            }
            // Nasdaq's public endpoint ignores the requested Main Market for
            // several legacy category labels that belong to First North.
            // Retain the server category filter but enforce venue locally.
            if item.market != "Main Market, Stockholm" {
                continue;
            }
            let publication_date = parse_nasdaq_publication_date(&item.published)?;
            if publication_date < start || publication_date > end {
                continue;
            }
            announcements.push(NasdaqCompanyAnnouncement {
                disclosure_id: item.disclosure_id,
                category_id: item.category_id,
                category: item.cns_category,
                headline: item.headline,
                language: item.language,
                message_url: item.message_url,
                published: item.published,
                publication_date,
                market: item.market,
                company: item.company,
            });
        }
        Ok((announcements, expected_pages.max(1)))
    }

    fn collect_all_nasdaq_news_interval(
        &self,
        cache_dir: &Path,
        start: Date,
        end: Date,
        pause_ms: u64,
    ) -> Result<(Vec<NasdaqCompanyAnnouncement>, usize), String> {
        const PAGE_SIZE: usize = 200;
        let first_bytes =
            self.all_nasdaq_news_page(cache_dir, start, end, 0, PAGE_SIZE, pause_ms)?;
        let first = parse_nasdaq_news_jsonp(&first_bytes)?;
        if first.count >= 10_000 {
            if start == end {
                return Err(format!(
                    "Nasdaq company news on {start} reached the 10,000-result count ceiling"
                ));
            }
            let span = (end - start).whole_days();
            let left_end = start + time::Duration::days(span / 2);
            let right_start = left_end
                .next_day()
                .ok_or("Nasdaq company-news date overflow")?;
            eprintln!(
                "Nasdaq company news: splitting all categories {start}..{end} at count ceiling"
            );
            let (mut left, left_pages) =
                self.collect_all_nasdaq_news_interval(cache_dir, start, left_end, pause_ms)?;
            let (right, right_pages) =
                self.collect_all_nasdaq_news_interval(cache_dir, right_start, end, pause_ms)?;
            left.extend(right);
            return Ok((left, left_pages + right_pages));
        }
        let expected_pages = first.count.div_ceil(PAGE_SIZE);
        let mut responses = vec![first];
        for page in 1..expected_pages {
            let offset = page * PAGE_SIZE;
            let bytes =
                self.all_nasdaq_news_page(cache_dir, start, end, offset, PAGE_SIZE, pause_ms)?;
            let response = parse_nasdaq_news_jsonp(&bytes)?;
            if response.results.item.is_empty() {
                return Err(format!(
                    "Nasdaq company news all categories {start}..{end} returned an empty page at offset {offset}"
                ));
            }
            responses.push(response);
        }
        let mut announcements = Vec::new();
        for item in responses
            .into_iter()
            .flat_map(|response| response.results.item)
        {
            if item.market != "Main Market, Stockholm" {
                continue;
            }
            let publication_date = parse_nasdaq_publication_date(&item.published)?;
            if publication_date < start || publication_date > end {
                continue;
            }
            announcements.push(NasdaqCompanyAnnouncement {
                disclosure_id: item.disclosure_id,
                category_id: item.category_id,
                category: item.cns_category,
                headline: item.headline,
                language: item.language,
                message_url: item.message_url,
                published: item.published,
                publication_date,
                market: item.market,
                company: item.company,
            });
        }
        Ok((announcements, expected_pages.max(1)))
    }

    #[allow(clippy::too_many_arguments)]
    fn all_nasdaq_news_page(
        &self,
        cache_dir: &Path,
        start: Date,
        end: Date,
        offset: usize,
        limit: usize,
        pause_ms: u64,
    ) -> Result<Vec<u8>, String> {
        let path = cache_dir.join(format!("all-{start}_{end}-offset-{offset:06}.jsonp"));
        if path.exists() {
            return std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()));
        }
        let from_boundary = start
            .previous_day()
            .ok_or("Nasdaq company-news start-date underflow")?;
        let from = news_boundary_milliseconds(from_boundary)?;
        let to = news_boundary_milliseconds(end)?;
        let mut last_error = String::new();
        for attempt in 1..=4_u64 {
            let result = self
                .client
                .get(NASDAQ_COMPANY_NEWS_API)
                .query(&[
                    ("callback", "handleResponse".to_string()),
                    ("countResults", "true".to_string()),
                    ("globalGroup", "exchangeNotice".to_string()),
                    ("displayLanguage", "en".to_string()),
                    ("timeZone", "CET".to_string()),
                    ("dateMask", "yyyy-MM-dd HH:mm:ss".to_string()),
                    ("limit", limit.to_string()),
                    ("start", offset.to_string()),
                    ("dir", "DESC".to_string()),
                    ("globalName", "NordicMainMarkets".to_string()),
                    ("market", "Main Market, Stockholm".to_string()),
                    ("fromDate", from.to_string()),
                    ("toDate", to.to_string()),
                ])
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .and_then(|response| response.bytes());
            match result {
                Ok(bytes) => {
                    let bytes = bytes.to_vec();
                    parse_nasdaq_news_jsonp(&bytes).map_err(|error| {
                        format!("Nasdaq company-news response validation failed: {error}")
                    })?;
                    std::fs::write(&path, &bytes)
                        .map_err(|error| format!("{}: {error}", path.display()))?;
                    if pause_ms > 0 {
                        std::thread::sleep(Duration::from_millis(pause_ms));
                    }
                    return Ok(bytes);
                }
                Err(error) => {
                    last_error = error.to_string();
                    if attempt < 4 {
                        std::thread::sleep(Duration::from_secs(attempt * 2));
                    }
                }
            }
        }
        Err(format!(
            "Nasdaq company news all categories {start}..{end} offset {offset} failed after 4 attempts: {last_error}"
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn nasdaq_news_page(
        &self,
        cache_dir: &Path,
        category_index: usize,
        category: &str,
        start: Date,
        end: Date,
        offset: usize,
        limit: usize,
        pause_ms: u64,
    ) -> Result<Vec<u8>, String> {
        let path = cache_dir.join(format!(
            "category-{category_index:02}-{start}_{end}-offset-{offset:06}.jsonp"
        ));
        if path.exists() {
            return std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()));
        }
        let from_boundary = start
            .previous_day()
            .ok_or("Nasdaq company-news start-date underflow")?;
        let from = news_boundary_milliseconds(from_boundary)?;
        let to = news_boundary_milliseconds(end)?;
        let mut last_error = String::new();
        for attempt in 1..=4_u64 {
            let result = self
                .client
                .get(NASDAQ_COMPANY_NEWS_API)
                .query(&[
                    ("callback", "handleResponse".to_string()),
                    ("countResults", "true".to_string()),
                    ("globalGroup", "exchangeNotice".to_string()),
                    ("displayLanguage", "en".to_string()),
                    ("timeZone", "CET".to_string()),
                    ("dateMask", "yyyy-MM-dd HH:mm:ss".to_string()),
                    ("limit", limit.to_string()),
                    ("start", offset.to_string()),
                    ("dir", "DESC".to_string()),
                    ("globalName", "NordicMainMarkets".to_string()),
                    ("market", "Main Market, Stockholm".to_string()),
                    ("cnsCategory", category.to_string()),
                    ("fromDate", from.to_string()),
                    ("toDate", to.to_string()),
                ])
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .and_then(|response| response.bytes());
            match result {
                Ok(bytes) => {
                    let bytes = bytes.to_vec();
                    parse_nasdaq_news_jsonp(&bytes).map_err(|error| {
                        format!("Nasdaq company-news response validation failed: {error}")
                    })?;
                    std::fs::write(&path, &bytes)
                        .map_err(|error| format!("{}: {error}", path.display()))?;
                    if pause_ms > 0 {
                        std::thread::sleep(Duration::from_millis(pause_ms));
                    }
                    return Ok(bytes);
                }
                Err(error) => {
                    last_error = error.to_string();
                    if attempt < 4 {
                        std::thread::sleep(Duration::from_secs(attempt * 2));
                    }
                }
            }
        }
        Err(format!(
            "Nasdaq company news {category:?} {start}..{end} offset {offset} failed after 4 attempts: {last_error}"
        ))
    }

    /// Archive Swedish ESEF annual filings and normalize only numeric,
    /// non-dimensional facts from the standard IFRS taxonomy. The public
    /// repository is a convenient mirror of official OAM submissions, not an
    /// authority publication clock. Consequently the repository's date-added
    /// value is used as a conservative lower bound and is never moved back to
    /// the financial period end.
    pub fn collect_esef_annual_filings(
        &self,
        root: &Path,
        company_news: &NasdaqCompanyNewsDataset,
        pause_ms: u64,
    ) -> Result<EsefAnnualCollection, String> {
        const PAGE_SIZE: usize = 200;
        let cache_dir = root.join("esef-sweden").join("raw");
        let metadata_dir = cache_dir.join("metadata");
        let filing_dir = cache_dir.join("filings");
        std::fs::create_dir_all(&metadata_dir).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&filing_dir).map_err(|error| error.to_string())?;

        let first = self.xbrl_api_page(&metadata_dir, 1, PAGE_SIZE, pause_ms)?;
        let filings_seen = first.meta.count;
        let pages = filings_seen.div_ceil(PAGE_SIZE);
        let mut responses = vec![first];
        for page in 2..=pages {
            responses.push(self.xbrl_api_page(&metadata_dir, page, PAGE_SIZE, pause_ms)?);
        }

        let mut official_dates = BTreeMap::<String, Vec<Date>>::new();
        for announcement in &company_news.announcements {
            if announcement
                .category
                .to_ascii_lowercase()
                .starts_with("annual")
            {
                official_dates
                    .entry(nasdaq_news_issuer_key(&announcement.company))
                    .or_default()
                    .push(announcement.publication_date);
            }
        }
        for dates in official_dates.values_mut() {
            dates.sort_unstable();
            dates.dedup();
        }

        let mut metadata = Vec::new();
        for response in responses {
            let entities = response
                .included
                .into_iter()
                .map(|entity| (entity.id, entity.attributes))
                .collect::<BTreeMap<_, _>>();
            for filing in response.data {
                metadata.push((filing, entities.clone()));
            }
        }
        metadata.sort_by(|(left, _), (right, _)| left.id.cmp(&right.id));

        let mut filings = Vec::new();
        let mut filings_failed = BTreeMap::new();
        let mut filings_without_json = 0_usize;
        for (index, (filing, entities)) in metadata.into_iter().enumerate() {
            let attributes = filing.attributes;
            let Some(json_path) = attributes.json_url else {
                filings_without_json += 1;
                continue;
            };
            let Some(entity) = entities.get(&filing.relationships.entity.data.id) else {
                filings_failed.insert(filing.id, "API response omitted the related entity".into());
                continue;
            };
            let report_period_end = match parse_iso_date_prefix(&attributes.period_end) {
                Ok(date) => date,
                Err(error) => {
                    filings_failed.insert(filing.id, error);
                    continue;
                }
            };
            let repository_date_added = match parse_iso_date_prefix(&attributes.date_added) {
                Ok(date) => date,
                Err(error) => {
                    filings_failed.insert(filing.id, error);
                    continue;
                }
            };
            if report_period_end >= repository_date_added
                || (repository_date_added - report_period_end).whole_days() > 730
            {
                filings_failed.insert(
                    filing.id,
                    format!(
                        "implausible report period {report_period_end} for repository date {repository_date_added}"
                    ),
                );
                continue;
            }
            let annual_deadline = report_period_end + time::Duration::days(270);
            let official_annual_report_date = official_dates
                .get(&nasdaq_news_issuer_key(&entity.name))
                .and_then(|dates| {
                    dates
                        .iter()
                        .copied()
                        .find(|date| *date > report_period_end && *date <= annual_deadline)
                });
            let available_date = official_annual_report_date
                .map(|date| date.max(repository_date_added))
                .unwrap_or(repository_date_added);
            let cache_path = filing_dir.join(format!("{}.json", filing.id));
            let bytes = if cache_path.exists() {
                match std::fs::read(&cache_path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        filings_failed.insert(filing.id, error.to_string());
                        continue;
                    }
                }
            } else {
                let url = format!("{XBRL_FILINGS_ORIGIN}{json_path}");
                match self.download_bytes_with_retries(&url, 4, 2) {
                    Ok(bytes) => {
                        if let Err(error) = std::fs::write(&cache_path, &bytes) {
                            filings_failed.insert(filing.id, error.to_string());
                            continue;
                        }
                        if pause_ms > 0 {
                            std::thread::sleep(Duration::from_millis(pause_ms));
                        }
                        bytes
                    }
                    Err(error) => {
                        filings_failed.insert(filing.id, error);
                        continue;
                    }
                }
            };
            let facts = match parse_esef_ifrs_facts(&bytes, report_period_end) {
                Ok(facts) if !facts.is_empty() => facts,
                Ok(_) => {
                    filings_failed.insert(filing.id, "no usable IFRS numeric facts".into());
                    continue;
                }
                Err(error) => {
                    filings_failed.insert(filing.id, error);
                    continue;
                }
            };
            filings.push(EsefAnnualFiling {
                filing_id: filing.id,
                entity_name: entity.name.clone(),
                lei: entity.identifier.clone(),
                report_period_end,
                repository_date_added,
                official_annual_report_date,
                available_date,
                json_url: format!("{XBRL_FILINGS_ORIGIN}{json_path}"),
                package_url: format!("{XBRL_FILINGS_ORIGIN}{}", attributes.package_url),
                sha256: attributes.sha256,
                error_count: attributes.error_count,
                warning_count: attributes.warning_count,
                inconsistency_count: attributes.inconsistency_count,
                facts,
            });
            if (index + 1) % 100 == 0 {
                eprintln!(
                    "ESEF Sweden: {}/{} metadata rows, {} parsed, {} failures",
                    index + 1,
                    filings_seen,
                    filings.len(),
                    filings_failed.len()
                );
            }
        }
        filings.sort_by(|left, right| {
            left.available_date
                .cmp(&right.available_date)
                .then_with(|| left.lei.cmp(&right.lei))
                .then_with(|| left.filing_id.cmp(&right.filing_id))
        });
        let now = OffsetDateTime::now_utc();
        let dataset = EsefAnnualDataset {
            format_version: "esef-sweden-ifrs-facts-1".into(),
            generated_at: now.to_string(),
            source: "XBRL International filings.xbrl.org mirror of Swedish ESEF/OAM filings"
                .into(),
            api_endpoint: XBRL_FILINGS_API.into(),
            country: "SE".into(),
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            pause_ms,
            limitations: vec![
                "filings.xbrl.org is a public aggregation service, not the Swedish official appointed mechanism; it documents that coverage can be incomplete and filings can contain errors".into(),
                "repository_date_added is an ingestion timestamp, not an issuer publication timestamp; available_date is never earlier than it and may therefore delay otherwise-public facts".into(),
                "official_annual_report_date is an exact normalized issuer-name join to Nasdaq Main Market Stockholm annual-report announcements and remains absent when that conservative join fails".into(),
                "Only finite numeric standard ifrs-full facts without segment or other dimensional qualifiers are retained; issuer-extension facts are excluded".into(),
                "Multiple filing versions are retained and become available independently; causal consumers must select only versions whose available_date precedes the decision date".into(),
                "ESEF applies only to recent annual financial reports and does not provide a long quarterly fundamental history".into(),
            ],
            filings_seen,
            filings_without_json,
            filings_failed,
            filings,
        };
        let snapshot_dir = root.join("esef-sweden").join("snapshots").join(format!(
            "{}-{}",
            now.date(),
            now.unix_timestamp()
        ));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset_path = snapshot_dir.join("annual-filings.json");
        write_json(&dataset_path, &dataset)?;
        write_json(
            &root.join("esef-sweden").join("latest-annual-filings.json"),
            &dataset,
        )?;
        Ok(EsefAnnualCollection {
            dataset_path,
            filings_seen: dataset.filings_seen,
            filings_parsed: dataset.filings.len(),
            entities: dataset
                .filings
                .iter()
                .map(|filing| filing.lei.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            facts: dataset
                .filings
                .iter()
                .map(|filing| filing.facts.len())
                .sum(),
            failures: dataset.filings_failed.len(),
        })
    }

    /// Collect the four predeclared official Riksbank series used for
    /// Stockholm macro/FX research. Four interval calls stay below the
    /// unregistered public limit of five calls per minute.
    pub fn collect_riksbank_stockholm_macro(
        &self,
        root: &Path,
        start: Date,
        end: Date,
    ) -> Result<RiksbankMacroCollection, String> {
        if end < start {
            return Err("Riksbank macro end precedes start".into());
        }
        let cache_dir = root.join("riksbank-macro").join("raw");
        std::fs::create_dir_all(&cache_dir).map_err(|error| error.to_string())?;
        let mut series = Vec::new();
        for (series_id, description, publication_time) in RIKSBANK_STOCKHOLM_MACRO_SERIES {
            let path = cache_dir.join(format!("{series_id}-{start}_{end}.json"));
            let bytes = if path.exists() {
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?
            } else {
                let suffix = format!("Observations/{series_id}/{start}/{end}");
                let primary = format!("{RIKSBANK_API}/{suffix}");
                let bytes = match self.download_bytes_with_retries(&primary, 2, 2) {
                    Ok(bytes) => bytes,
                    Err(primary_error) => {
                        let fallback = format!("{RIKSBANK_API_FALLBACK}/{suffix}");
                        self.download_bytes_with_retries(&fallback, 2, 2).map_err(|error| {
                            format!(
                                "Riksbank {series_id} failed through primary ({primary_error}) and official APIM alias ({error})"
                            )
                        })?
                    }
                };
                parse_riksbank_observations(&bytes, start, end)?;
                std::fs::write(&path, &bytes)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                bytes
            };
            let observations = parse_riksbank_observations(&bytes, start, end)?;
            if observations.is_empty() {
                return Err(format!("Riksbank {series_id} returned no observations"));
            }
            series.push(RiksbankSeries {
                series_id: (*series_id).into(),
                description: (*description).into(),
                publication_time: (*publication_time).into(),
                observations,
            });
        }
        let now = OffsetDateTime::now_utc();
        let dataset = RiksbankMacroDataset {
            format_version: "riksbank-stockholm-macro-1".into(),
            generated_at: now.to_string(),
            source: "Sveriges Riksbank".into(),
            api_endpoint: RIKSBANK_API.into(),
            requested_start: start,
            requested_end: end,
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            limitations: vec![
                "The Riksbank states that exchange rates are indicative and can be corrected after publication; this archive is not a point-in-time revision-vintage database".into(),
                "Exchange rates are published at 16:15 and the policy rate series at 09:10 Europe/Stockholm; same-date values are usable only for decisions after those publication times".into(),
                "The official series are macro context, not executable FX prices".into(),
                "The public API permits five unregistered calls per minute and 1,000 per day; this collector makes exactly four interval calls when the raw cache is empty".into(),
            ],
            series,
        };
        let snapshot_dir = root.join("riksbank-macro").join("snapshots").join(format!(
            "{}-{}",
            now.date(),
            now.unix_timestamp()
        ));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset_path = snapshot_dir.join("stockholm-macro.json");
        write_json(&dataset_path, &dataset)?;
        write_json(
            &root
                .join("riksbank-macro")
                .join("latest-stockholm-macro.json"),
            &dataset,
        )?;
        Ok(RiksbankMacroCollection {
            dataset_path,
            series: dataset.series.len(),
            observations: dataset
                .series
                .iter()
                .map(|series| series.observations.len())
                .sum(),
        })
    }

    /// Collect licensed EOD histories for provider-inactive Stockholm common
    /// stocks, restricted to ISINs present in official Nasdaq delisting
    /// notices. The token is accepted by the shared provider adapter and is
    /// never persisted in raw URLs, manifests, or bot configuration.
    #[allow(clippy::too_many_arguments)]
    pub fn collect_eodhd_stockholm_delisted(
        &self,
        root: &Path,
        official_notices_path: &Path,
        official_notices: &NasdaqEquityNoticeDataset,
        api_token: &str,
        start: Date,
        end: Date,
        pause_ms: u64,
        limit: usize,
    ) -> Result<EodhdStockholmDelistedCollection, String> {
        if api_token.trim().is_empty() {
            return Err("EODHD_API_TOKEN is empty".into());
        }
        if end < start {
            return Err("EODHD delisted-history end precedes start".into());
        }
        let cache_dir = root.join("eodhd-stockholm-delisted").join("raw");
        let history_dir = cache_dir.join("eod");
        std::fs::create_dir_all(&history_dir).map_err(|error| error.to_string())?;
        let symbol_path = cache_dir.join("ST-delisted-common-stock.json");
        let symbol_bytes = if symbol_path.exists() {
            std::fs::read(&symbol_path)
                .map_err(|error| format!("{}: {error}", symbol_path.display()))?
        } else {
            let bytes = self
                .client
                .get(format!("{EODHD_API}/exchange-symbol-list/ST"))
                .query(&[
                    ("delisted", "1"),
                    ("type", "common_stock"),
                    ("fmt", "json"),
                    ("api_token", api_token),
                ])
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .and_then(|response| response.bytes())
                .map_err(|error| format!("EODHD Stockholm delisted symbols: {error}"))?
                .to_vec();
            parse_eodhd_symbols(&bytes)?;
            std::fs::write(&symbol_path, &bytes)
                .map_err(|error| format!("{}: {error}", symbol_path.display()))?;
            bytes
        };
        let provider_symbols = parse_eodhd_symbols(&symbol_bytes)?;
        let mut official_by_isin = BTreeMap::<String, Vec<&NasdaqEquityNotice>>::new();
        for notice in &official_notices.notices {
            if notice.event_kind == NasdaqEquityNoticeKind::Delisting {
                for isin in &notice.isins {
                    if valid_isin(isin) {
                        official_by_isin
                            .entry(isin.clone())
                            .or_default()
                            .push(notice);
                    }
                }
            }
        }
        let official_isins = official_by_isin.len();
        let mut matched = provider_symbols
            .iter()
            .filter(|symbol| official_by_isin.contains_key(&symbol.isin))
            .cloned()
            .collect::<Vec<_>>();
        matched.sort_by(|a, b| a.isin.cmp(&b.isin).then_with(|| a.code.cmp(&b.code)));
        matched.dedup_by(|right, left| right.isin == left.isin && right.code == left.code);
        let matched_isins = matched
            .iter()
            .map(|symbol| symbol.isin.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        if limit > 0 {
            matched.truncate(limit);
        }
        let mut failures = BTreeMap::new();
        let mut histories = Vec::new();
        for (index, symbol) in matched.iter().enumerate() {
            let safe_code = symbol
                .code
                .replace(|character: char| !character.is_ascii_alphanumeric(), "_");
            let raw_path = history_dir.join(format!("{safe_code}-ST-{start}_{end}.json"));
            let bytes = if raw_path.exists() {
                std::fs::read(&raw_path).map_err(|error| format!("{}: {error}", raw_path.display()))
            } else {
                let result = self
                    .client
                    .get(format!("{EODHD_API}/eod/{}.ST", symbol.code))
                    .query(&[
                        ("from", start.to_string()),
                        ("to", end.to_string()),
                        ("period", "d".into()),
                        ("order", "a".into()),
                        ("fmt", "json".into()),
                        ("api_token", api_token.into()),
                    ])
                    .send()
                    .and_then(reqwest::blocking::Response::error_for_status)
                    .and_then(|response| response.bytes())
                    .map_err(|error| format!("EODHD {}.ST: {error}", symbol.code))
                    .map(|bytes| bytes.to_vec());
                if pause_ms > 0 {
                    std::thread::sleep(Duration::from_millis(pause_ms));
                }
                result.and_then(|bytes| {
                    parse_eodhd_bars(&bytes, start, end)?;
                    std::fs::write(&raw_path, &bytes)
                        .map_err(|error| format!("{}: {error}", raw_path.display()))?;
                    Ok(bytes)
                })
            };
            match bytes.and_then(|bytes| parse_eodhd_bars(&bytes, start, end)) {
                Ok(bars) if !bars.is_empty() => {
                    let notices = &official_by_isin[&symbol.isin];
                    histories.push(EodhdDelistedHistory {
                        symbol: symbol.clone(),
                        official_notice_ids: notices
                            .iter()
                            .map(|notice| notice.disclosure_id)
                            .collect(),
                        official_last_trading_date: notices
                            .iter()
                            .filter_map(|notice| notice.last_trading_date)
                            .max(),
                        bars,
                    });
                }
                Ok(_) => {
                    failures.insert(symbol.isin.clone(), "EODHD returned no valid bars".into());
                }
                Err(error) => {
                    failures.insert(symbol.isin.clone(), error);
                }
            }
            if (index + 1) % 25 == 0 || index + 1 == matched.len() {
                eprintln!(
                    "EODHD Stockholm delisted: {}/{}, {} histories, {} failures",
                    index + 1,
                    matched.len(),
                    histories.len(),
                    failures.len()
                );
            }
        }
        histories.sort_by(|a, b| a.symbol.isin.cmp(&b.symbol.isin));
        let now = OffsetDateTime::now_utc();
        let dataset = EodhdStockholmDelistedDataset {
            format_version: "eodhd-stockholm-official-delistings-1".into(),
            generated_at: now.to_string(),
            provider: "EODHD licensed EOD API".into(),
            exchange_code: "ST".into(),
            operating_mic: "XSTO".into(),
            official_notice_source: official_notices_path.to_string_lossy().into_owned(),
            requested_start: start,
            requested_end: end,
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            limitations: vec![
                "Provider inactive common stocks are admitted only when their checksum-valid ISIN appears in an official Nasdaq Stockholm delisting notice".into(),
                "A delisting notice proves an event and identifier, not historical Large/Mid/Small membership; point-in-time size segment still requires reference data".into(),
                "EODHD adjusted_close is provider-adjusted; corporate-action reconciliation against official notices remains required before promotion".into(),
                "Delisting due to acquisition differs economically from bankruptcy; terminal outcomes must be modeled explicitly rather than treating all missing post-delist prices as zero".into(),
            ],
            provider_delisted_symbols: provider_symbols.len(),
            official_delisting_isins: official_isins,
            matched_isins,
            failures,
            histories,
        };
        let snapshot_dir = root
            .join("eodhd-stockholm-delisted")
            .join("snapshots")
            .join(format!("{}-{}", now.date(), now.unix_timestamp()));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset_path = snapshot_dir.join("official-delistings.json");
        write_json(&dataset_path, &dataset)?;
        write_json(
            &root
                .join("eodhd-stockholm-delisted")
                .join("latest-official-delistings.json"),
            &dataset,
        )?;
        Ok(EodhdStockholmDelistedCollection {
            dataset_path,
            provider_symbols: dataset.provider_delisted_symbols,
            official_isins: dataset.official_delisting_isins,
            matched_isins: dataset.matched_isins,
            histories: dataset.histories.len(),
            bars: dataset
                .histories
                .iter()
                .map(|history| history.bars.len())
                .sum(),
            failures: dataset.failures.len(),
        })
    }

    /// Collect licensed point-in-time quarterly statements for current Main
    /// Market securities and officially noticed Stockholm delistings. Stable
    /// ISIN matching is completed here so no vendor symbol logic enters a bot.
    #[allow(clippy::too_many_arguments)]
    pub fn collect_eodhd_stockholm_fundamentals(
        &self,
        root: &Path,
        universe_path: &Path,
        universe: &[Instrument],
        official_notices_path: &Path,
        official_notices: &NasdaqEquityNoticeDataset,
        api_token: &str,
        pause_ms: u64,
        limit: usize,
    ) -> Result<EodhdStockholmFundamentalCollection, String> {
        if api_token.trim().is_empty() {
            return Err("EODHD_API_TOKEN is empty".into());
        }
        let cache_dir = root.join("eodhd-stockholm-fundamentals").join("raw");
        let response_dir = cache_dir.join("fundamentals");
        std::fs::create_dir_all(&response_dir).map_err(|error| error.to_string())?;
        let active_path = cache_dir.join("ST-active-common-stock.json");
        let inactive_path = cache_dir.join("ST-delisted-common-stock.json");
        let active = self.eodhd_symbol_list(&active_path, api_token, false)?;
        let inactive = self.eodhd_symbol_list(&inactive_path, api_token, true)?;
        // Keep the complete vendor count for provenance, but choose one code
        // per ISIN with an active listing preferred over an inactive alias.
        // Sorting a combined list by code would otherwise make the choice
        // accidental and could pin a current security to a stale endpoint.
        let mut provider_symbols = active
            .iter()
            .chain(inactive.iter())
            .cloned()
            .collect::<Vec<_>>();
        provider_symbols.sort_by(|left, right| {
            left.isin
                .cmp(&right.isin)
                .then_with(|| left.code.cmp(&right.code))
        });
        provider_symbols.dedup_by(|right, left| right.isin == left.isin && right.code == left.code);

        let mut target_isins = universe
            .iter()
            .filter(|instrument| {
                matches!(
                    instrument.bucket,
                    UniverseBucket::LargeCap | UniverseBucket::MidCap | UniverseBucket::SmallCap
                ) && valid_isin(&instrument.isin)
            })
            .map(|instrument| instrument.isin.clone())
            .collect::<BTreeSet<_>>();
        for notice in &official_notices.notices {
            if notice.body_mentions_stockholm
                && notice.event_kind == NasdaqEquityNoticeKind::Delisting
            {
                target_isins.extend(notice.isins.iter().filter(|isin| valid_isin(isin)).cloned());
            }
        }
        let mut matched = preferred_eodhd_symbols(&target_isins, active, inactive);
        let matched_isins = matched.len();
        if limit > 0 {
            matched.truncate(limit);
        }

        let mut failures = BTreeMap::new();
        let mut histories = Vec::new();
        for (index, symbol) in matched.iter().enumerate() {
            let safe_code = symbol
                .code
                .replace(|character: char| !character.is_ascii_alphanumeric(), "_");
            let raw_path = response_dir.join(format!("{safe_code}-ST.json"));
            let bytes = if raw_path.exists() {
                std::fs::read(&raw_path).map_err(|error| format!("{}: {error}", raw_path.display()))
            } else {
                let result = self
                    .client
                    .get(format!("{EODHD_API}/v1.1/fundamentals/{}.ST", symbol.code))
                    .query(&[("fmt", "json"), ("api_token", api_token)])
                    .send()
                    .and_then(reqwest::blocking::Response::error_for_status)
                    .and_then(|response| response.bytes())
                    .map_err(|error| format!("EODHD fundamentals {}.ST: {error}", symbol.code))
                    .map(|bytes| bytes.to_vec());
                if pause_ms > 0 {
                    std::thread::sleep(Duration::from_millis(pause_ms));
                }
                result.and_then(|bytes| {
                    parse_eodhd_quarterly_fundamentals(symbol, &bytes)?;
                    std::fs::write(&raw_path, &bytes)
                        .map_err(|error| format!("{}: {error}", raw_path.display()))?;
                    Ok(bytes)
                })
            };
            match bytes.and_then(|bytes| parse_eodhd_quarterly_fundamentals(symbol, &bytes)) {
                Ok(quarterly) if !quarterly.is_empty() => {
                    histories.push(EodhdFundamentalHistory {
                        symbol: symbol.clone(),
                        quarterly,
                    });
                }
                Ok(_) => {
                    failures.insert(
                        symbol.isin.clone(),
                        "EODHD returned no causal quarterly filings".into(),
                    );
                }
                Err(error) => {
                    failures.insert(symbol.isin.clone(), error);
                }
            }
            if (index + 1) % 25 == 0 || index + 1 == matched.len() {
                eprintln!(
                    "EODHD Stockholm fundamentals: {}/{}, {} histories, {} failures",
                    index + 1,
                    matched.len(),
                    histories.len(),
                    failures.len()
                );
            }
        }
        histories.sort_by(|left, right| left.symbol.isin.cmp(&right.symbol.isin));
        let now = OffsetDateTime::now_utc();
        let dataset = EodhdStockholmFundamentalDataset {
            format_version: "eodhd-stockholm-quarterly-fundamentals-1".into(),
            generated_at: now.to_string(),
            provider: "EODHD licensed Fundamentals API v1.1".into(),
            endpoint: "https://eodhd.com/api/v1.1/fundamentals/{ticker}.ST".into(),
            exchange_code: "ST".into(),
            operating_mic: "XSTO".into(),
            universe_source: universe_path.to_string_lossy().into_owned(),
            official_notice_source: official_notices_path.to_string_lossy().into_owned(),
            raw_cache_dir: cache_dir.to_string_lossy().into_owned(),
            pause_ms,
            limitations: vec![
                "Every accounting value is withheld until the provider filing_date; period-end dates are never used as availability dates".into(),
                "Current Main Market securities are matched by checksum-valid ISIN. Inactive candidates additionally require an official Nasdaq Stockholm delisting notice".into(),
                "Provider statement history can include later restatements. Using the row's filing_date is conservative but cannot reconstruct values that a provider overwrote without a revision archive".into(),
                "Quarterly statement durations may be quarter-only or year-to-date depending on issuer reporting. Ratios and year-over-year comparisons must retain this limitation".into(),
                "Official point-in-time Large/Mid/Small segment membership and terminal delisting outcomes remain separate required datasets".into(),
            ],
            provider_symbols: provider_symbols.len(),
            target_isins: target_isins.len(),
            matched_isins,
            failures,
            histories,
        };
        let snapshot_dir = root
            .join("eodhd-stockholm-fundamentals")
            .join("snapshots")
            .join(format!("{}-{}", now.date(), now.unix_timestamp()));
        std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
        let dataset_path = snapshot_dir.join("quarterly-fundamentals.json");
        write_json(&dataset_path, &dataset)?;
        write_json(
            &root
                .join("eodhd-stockholm-fundamentals")
                .join("latest-quarterly-fundamentals.json"),
            &dataset,
        )?;
        Ok(EodhdStockholmFundamentalCollection {
            dataset_path,
            provider_symbols: dataset.provider_symbols,
            target_isins: dataset.target_isins,
            matched_isins: dataset.matched_isins,
            histories: dataset.histories.len(),
            quarterly_filings: dataset
                .histories
                .iter()
                .map(|history| history.quarterly.len())
                .sum(),
            failures: dataset.failures.len(),
        })
    }

    fn eodhd_symbol_list(
        &self,
        path: &Path,
        api_token: &str,
        delisted: bool,
    ) -> Result<Vec<EodhdDelistedSymbol>, String> {
        let bytes = if path.exists() {
            std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?
        } else {
            let mut request = self
                .client
                .get(format!("{EODHD_API}/exchange-symbol-list/ST"))
                .query(&[
                    ("type", "common_stock"),
                    ("fmt", "json"),
                    ("api_token", api_token),
                ]);
            if delisted {
                request = request.query(&[("delisted", "1")]);
            }
            let bytes = request
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .and_then(|response| response.bytes())
                .map_err(|error| format!("EODHD Stockholm symbol list: {error}"))?
                .to_vec();
            parse_eodhd_symbols(&bytes)?;
            std::fs::write(path, &bytes).map_err(|error| format!("{}: {error}", path.display()))?;
            bytes
        };
        parse_eodhd_symbols(&bytes)
    }

    fn xbrl_api_page(
        &self,
        cache_dir: &Path,
        page: usize,
        page_size: usize,
        pause_ms: u64,
    ) -> Result<XbrlApiResponse, String> {
        let path = cache_dir.join(format!("page-{page:04}.json"));
        let bytes = if path.exists() {
            std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?
        } else {
            let bytes = self
                .client
                .get(XBRL_FILINGS_API)
                .query(&[
                    ("filter[country]", "SE".to_string()),
                    ("page[size]", page_size.to_string()),
                    ("page[number]", page.to_string()),
                    ("include", "entity".to_string()),
                ])
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .and_then(|response| response.bytes())
                .map_err(|error| format!("XBRL filings API page {page}: {error}"))?
                .to_vec();
            serde_json::from_slice::<XbrlApiResponse>(&bytes)
                .map_err(|error| format!("XBRL filings API page {page}: {error}"))?;
            std::fs::write(&path, &bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if pause_ms > 0 {
                std::thread::sleep(Duration::from_millis(pause_ms));
            }
            bytes
        };
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("XBRL filings API page {page}: {error}"))
    }

    fn download_bytes_with_retries(
        &self,
        url: &str,
        attempts: u64,
        retry_delay_seconds: u64,
    ) -> Result<Vec<u8>, String> {
        let mut last_error = String::new();
        for attempt in 1..=attempts {
            match self
                .client
                .get(url)
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .and_then(|response| response.bytes())
            {
                Ok(bytes) => return Ok(bytes.to_vec()),
                Err(error) => {
                    last_error = error.to_string();
                    if attempt < attempts {
                        std::thread::sleep(Duration::from_secs(
                            retry_delay_seconds.saturating_mul(attempt),
                        ));
                    }
                }
            }
        }
        Err(format!(
            "{url} failed after {attempts} attempts: {last_error}"
        ))
    }

    fn download_bounded_bytes_with_retries(
        &self,
        url: &str,
        max_bytes: u64,
        attempts: u64,
        retry_delay_seconds: u64,
    ) -> Result<Vec<u8>, String> {
        let mut last_error = String::new();
        for attempt in 1..=attempts {
            let result = (|| {
                let response = self
                    .client
                    .get(url)
                    .send()
                    .and_then(reqwest::blocking::Response::error_for_status)
                    .map_err(|error| error.to_string())?;
                if response
                    .content_length()
                    .is_some_and(|length| length > max_bytes)
                {
                    return Err(format!(
                        "attachment declares more than the {max_bytes}-byte ceiling"
                    ));
                }
                let mut bytes = Vec::new();
                response
                    .take(max_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                if bytes.len() as u64 > max_bytes {
                    return Err(format!("attachment exceeds the {max_bytes}-byte ceiling"));
                }
                Ok(bytes)
            })();
            match result {
                Ok(bytes) => return Ok(bytes),
                Err(error) => {
                    last_error = error;
                    if last_error.contains("byte ceiling") {
                        break;
                    }
                    if attempt < attempts {
                        std::thread::sleep(Duration::from_secs(
                            retry_delay_seconds.saturating_mul(attempt),
                        ));
                    }
                }
            }
        }
        Err(format!(
            "attachment download failed after at most {attempts} attempts: {last_error}"
        ))
    }

    fn download_bytes_with_query_retries(
        &self,
        url: &str,
        query: &[(&str, String)],
        attempts: u64,
        retry_delay_seconds: u64,
    ) -> Result<Vec<u8>, String> {
        let mut last_error = String::new();
        for attempt in 1..=attempts {
            match self
                .client
                .get(url)
                .query(query)
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .and_then(|response| response.bytes())
            {
                Ok(bytes) => return Ok(bytes.to_vec()),
                Err(error) => {
                    last_error = error.to_string();
                    if attempt < attempts {
                        std::thread::sleep(Duration::from_secs(
                            retry_delay_seconds.saturating_mul(attempt),
                        ));
                    }
                }
            }
        }
        Err(format!(
            "{url} failed after {attempts} attempts: {last_error}"
        ))
    }
}

pub fn collect_fi_net_shorts(root: &Path) -> Result<FiNetShortCollection, String> {
    PublicEquityData::new()?.collect_fi_net_shorts(root)
}

pub fn collect_skv_equity_history_catalogue(
    root: &Path,
) -> Result<SkvEquityHistoryCollection, String> {
    PublicEquityData::new()?.collect_skv_equity_history_catalogue(root)
}

pub fn load_skv_equity_history_catalogue(path: &Path) -> Result<SkvEquityHistoryCatalogue, String> {
    read_json(path)
}

pub fn load_skv_listing_history(path: &Path) -> Result<SkvListingHistoryDataset, String> {
    read_json(path)
}

/// Return the start of the current continuous Stockholm Main Market spell for
/// each issuer key supported by Skatteverket's effective-dated history. An
/// explicit delisting or move to First North/another venue resets an older
/// admission; later share-class admissions do not shorten an uninterrupted
/// issuer-level Main Market spell.
pub fn skv_current_main_market_admission_dates(
    dataset: &SkvListingHistoryDataset,
) -> BTreeMap<String, Date> {
    let mut by_issuer = BTreeMap::<String, Vec<&SkvListingHistoryRow>>::new();
    for row in &dataset.rows {
        let key = stockholm_security_issuer_key(&row.company_name);
        if !key.is_empty() && row.effective_date.is_some() {
            by_issuer.entry(key).or_default().push(row);
        }
    }
    by_issuer
        .into_iter()
        .filter_map(|(issuer, mut rows)| {
            rows.sort_by_key(|row| row.effective_date);
            let last_exit = rows
                .iter()
                .filter(|row| {
                    row.event_kind == SkvListingEventKind::Delisting
                        || matches!(
                            (row.event_kind, row.market_hint),
                            (
                                SkvListingEventKind::Listing | SkvListingEventKind::ListChange,
                                SkvMarketHint::FirstNorth | SkvMarketHint::OtherSwedishVenue
                            )
                        )
                })
                .filter_map(|row| row.effective_date)
                .max();
            rows.into_iter()
                .filter(|row| {
                    matches!(
                        row.event_kind,
                        SkvListingEventKind::Listing | SkvListingEventKind::ListChange
                    ) && row.market_hint == SkvMarketHint::StockholmMainMarket
                })
                .filter_map(|row| row.effective_date)
                .filter(|date| last_exit.is_none_or(|exit| *date > exit))
                .min()
                .map(|date| (issuer, date))
        })
        .collect()
}

pub fn collect_skv_listing_history(
    root: &Path,
    catalogue: &SkvEquityHistoryCatalogue,
    pause_ms: u64,
    limit: usize,
) -> Result<SkvListingHistoryCollection, String> {
    PublicEquityData::new()?.collect_skv_listing_history(root, catalogue, pause_ms, limit)
}

pub fn collect_fi_pdmr(
    root: &Path,
    start: Date,
    end: Date,
    pause_ms: u64,
    interval_days: usize,
) -> Result<FiPdmrCollection, String> {
    PublicEquityData::new()?.collect_fi_pdmr(root, start, end, pause_ms, interval_days)
}

pub fn collect_nasdaq_financial_reports(
    root: &Path,
    start: Date,
    end: Date,
    pause_ms: u64,
) -> Result<NasdaqCompanyNewsCollection, String> {
    PublicEquityData::new()?.collect_nasdaq_financial_reports(root, start, end, pause_ms)
}

pub fn collect_nasdaq_stockholm_company_news(
    root: &Path,
    start: Date,
    end: Date,
    pause_ms: u64,
) -> Result<NasdaqCompanyNewsCollection, String> {
    PublicEquityData::new()?.collect_nasdaq_stockholm_company_news(root, start, end, pause_ms)
}

pub fn collect_nasdaq_financial_report_messages(
    root: &Path,
    metadata_source: &Path,
    metadata: &NasdaqCompanyNewsDataset,
    pause_ms: u64,
    concurrency: usize,
    limit: usize,
) -> Result<NasdaqFinancialReportMessageCollection, String> {
    PublicEquityData::new()?.collect_nasdaq_financial_report_messages(
        root,
        metadata_source,
        metadata,
        pause_ms,
        concurrency,
        limit,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn collect_nasdaq_financial_report_attachments(
    root: &Path,
    message_metadata_source: &Path,
    metadata: &NasdaqFinancialReportMessageDataset,
    pause_ms: u64,
    concurrency: usize,
    max_attachment_bytes: u64,
    limit: usize,
    cached_only: bool,
) -> Result<NasdaqFinancialReportAttachmentCollection, String> {
    PublicEquityData::new()?.collect_nasdaq_financial_report_attachments(
        root,
        message_metadata_source,
        metadata,
        pause_ms,
        concurrency,
        max_attachment_bytes,
        limit,
        cached_only,
    )
}

pub fn collect_nasdaq_stockholm_equity_notices(
    root: &Path,
    start: Date,
    end: Date,
    pause_ms: u64,
) -> Result<NasdaqEquityNoticeCollection, String> {
    PublicEquityData::new()?.collect_nasdaq_stockholm_equity_notices(root, start, end, pause_ms)
}

pub fn collect_nasdaq_stockholm_market_history(
    root: &Path,
    start: Date,
    end: Date,
    pause_ms: u64,
    limit: usize,
    supplemental_universe: &[Instrument],
) -> Result<NasdaqMarketHistoryCollection, String> {
    PublicEquityData::new()?.collect_nasdaq_stockholm_market_history(
        root,
        start,
        end,
        pause_ms,
        limit,
        supplemental_universe,
    )
}

pub fn collect_esef_annual_filings(
    root: &Path,
    company_news: &NasdaqCompanyNewsDataset,
    pause_ms: u64,
) -> Result<EsefAnnualCollection, String> {
    PublicEquityData::new()?.collect_esef_annual_filings(root, company_news, pause_ms)
}

pub fn collect_riksbank_stockholm_macro(
    root: &Path,
    start: Date,
    end: Date,
) -> Result<RiksbankMacroCollection, String> {
    PublicEquityData::new()?.collect_riksbank_stockholm_macro(root, start, end)
}

#[allow(clippy::too_many_arguments)]
pub fn collect_eodhd_stockholm_delisted(
    root: &Path,
    official_notices_path: &Path,
    official_notices: &NasdaqEquityNoticeDataset,
    api_token: &str,
    start: Date,
    end: Date,
    pause_ms: u64,
    limit: usize,
) -> Result<EodhdStockholmDelistedCollection, String> {
    PublicEquityData::new()?.collect_eodhd_stockholm_delisted(
        root,
        official_notices_path,
        official_notices,
        api_token,
        start,
        end,
        pause_ms,
        limit,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn collect_eodhd_stockholm_fundamentals(
    root: &Path,
    universe_path: &Path,
    universe: &[Instrument],
    official_notices_path: &Path,
    official_notices: &NasdaqEquityNoticeDataset,
    api_token: &str,
    pause_ms: u64,
    limit: usize,
) -> Result<EodhdStockholmFundamentalCollection, String> {
    PublicEquityData::new()?.collect_eodhd_stockholm_fundamentals(
        root,
        universe_path,
        universe,
        official_notices_path,
        official_notices,
        api_token,
        pause_ms,
        limit,
    )
}

pub fn load_eodhd_stockholm_delisted(path: &Path) -> Result<EodhdStockholmDelistedDataset, String> {
    read_json(path)
}

pub fn load_eodhd_stockholm_fundamentals(
    path: &Path,
) -> Result<EodhdStockholmFundamentalDataset, String> {
    read_json(path)
}

pub fn collect_nasdaq_benchmark(
    root: &Path,
    symbol: &str,
    start: Date,
    end: Date,
) -> Result<BenchmarkHistory, String> {
    let history = PublicEquityData::new()?.nasdaq_index_history(symbol, start, end)?;
    let directory = root.join("benchmarks");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    write_json(&directory.join(format!("{symbol}.json")), &history)?;
    Ok(history)
}

pub fn load_benchmark(path: &Path) -> Result<BenchmarkHistory, String> {
    read_json(path)
}

pub fn load_nasdaq_equity_notices(path: &Path) -> Result<NasdaqEquityNoticeDataset, String> {
    read_json(path)
}

pub fn load_nasdaq_financial_report_messages(
    path: &Path,
) -> Result<NasdaqFinancialReportMessageDataset, String> {
    read_json(path)
}

pub fn load_nasdaq_financial_report_attachments(
    path: &Path,
) -> Result<NasdaqFinancialReportAttachmentDataset, String> {
    read_json(path)
}

pub fn load_nasdaq_market_history(
    root: &Path,
) -> Result<
    (
        NasdaqMarketHistoryManifest,
        Vec<NasdaqInstrumentMarketHistory>,
    ),
    String,
> {
    let dataset_dir = root.join("nasdaq-market-history");
    let manifest = read_json(&dataset_dir.join("latest-manifest.json"))?;
    let universe: Vec<Instrument> = read_json(&dataset_dir.join("universe.json"))?;
    let mut histories = Vec::new();
    for instrument in universe {
        let path = dataset_dir
            .join("bars")
            .join(format!("{}.json", instrument.orderbook_id));
        if path.exists() {
            histories.push(read_json(&path)?);
        }
    }
    Ok((manifest, histories))
}

pub fn load_instruments(path: &Path) -> Result<Vec<Instrument>, String> {
    read_json(path)
}

pub fn load_fi_net_shorts(path: &Path) -> Result<FiNetShortDataset, String> {
    read_json(path)
}

pub fn load_fi_pdmr(path: &Path) -> Result<FiPdmrDataset, String> {
    read_json(path)
}

pub fn load_nasdaq_company_news(path: &Path) -> Result<NasdaqCompanyNewsDataset, String> {
    read_json(path)
}

pub fn load_esef_annual_filings(path: &Path) -> Result<EsefAnnualDataset, String> {
    read_json(path)
}

pub fn load_riksbank_stockholm_macro(path: &Path) -> Result<RiksbankMacroDataset, String> {
    read_json(path)
}

pub fn normalize_esef_annual_fundamentals(filing: &EsefAnnualFiling) -> AnnualFundamentals {
    let currency = dominant_reporting_currency(filing);
    let currency_unit = currency.as_deref();
    let duration = |concepts: &[&str], prior: bool| {
        esef_statement_value(filing, concepts, currency_unit, true, prior)
    };
    let instant = |concepts: &[&str], prior: bool| {
        esef_statement_value(filing, concepts, currency_unit, false, prior)
    };
    let shares = esef_statement_value(
        filing,
        &["WeightedAverageShares"],
        Some("xbrli:shares"),
        true,
        false,
    )
    .or_else(|| {
        esef_statement_value(
            filing,
            &["AdjustedWeightedAverageShares"],
            Some("xbrli:shares"),
            true,
            false,
        )
    });
    let eps_unit = currency.as_ref().map(|unit| format!("{unit}/xbrli:shares"));
    AnnualFundamentals {
        reporting_currency: currency.clone(),
        revenue: duration(
            &[
                "Revenue",
                "RevenueFromContractsWithCustomers",
                "RevenueAndOperatingIncome",
            ],
            false,
        ),
        prior_revenue: duration(
            &[
                "Revenue",
                "RevenueFromContractsWithCustomers",
                "RevenueAndOperatingIncome",
            ],
            true,
        ),
        operating_profit: duration(&["ProfitLossFromOperatingActivities"], false),
        net_income: duration(&["ProfitLoss"], false),
        prior_net_income: duration(&["ProfitLoss"], true),
        assets: instant(&["Assets"], false),
        prior_assets: instant(&["Assets"], true),
        equity: instant(&["Equity", "EquityAttributableToOwnersOfParent"], false),
        prior_equity: instant(&["Equity", "EquityAttributableToOwnersOfParent"], true),
        cash: instant(&["CashAndCashEquivalents"], false),
        operating_cash_flow: duration(&["CashFlowsFromUsedInOperatingActivities"], false),
        current_assets: instant(&["CurrentAssets"], false),
        current_liabilities: instant(&["CurrentLiabilities"], false),
        basic_eps: eps_unit.as_deref().and_then(|unit| {
            esef_statement_value(
                filing,
                &[
                    "BasicEarningsLossPerShare",
                    "BasicAndDilutedEarningsLossPerShare",
                    "BasicEarningsLossPerShareFromContinuingOperations",
                ],
                Some(unit),
                true,
                false,
            )
        }),
        weighted_average_shares: shares,
    }
}

/// Conservative issuer-name join keys for Nasdaq's current security display
/// names and company-news issuer names. The public news response lacks ISINs,
/// so every downstream use must still report unmatched and ambiguous coverage.
pub fn stockholm_security_issuer_key(value: &str) -> String {
    let mut tokens = issuer_tokens(value);
    while tokens.last().is_some_and(|token| {
        matches!(
            token.as_str(),
            "a" | "b" | "c" | "d" | "pref" | "preference" | "sdb" | "sdr"
        )
    }) {
        tokens.pop();
    }
    tokens.join(" ")
}

pub fn nasdaq_news_issuer_key(value: &str) -> String {
    issuer_tokens(value).join(" ")
}

fn issuer_tokens(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .filter(|token| {
            !matches!(
                *token,
                "ab" | "aktiebolag" | "publ" | "plc" | "oyj" | "asa" | "limited" | "ltd"
            )
        })
        .map(str::to_owned)
        .collect()
}

fn dominant_reporting_currency(filing: &EsefAnnualFiling) -> Option<String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for fact in &filing.facts {
        if fact.period_end == filing.report_period_end
            && fact.unit.starts_with("iso4217:")
            && !fact.unit.contains('/')
        {
            *counts.entry(&fact.unit).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|(left_unit, left_count), (right_unit, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| right_unit.cmp(left_unit))
        })
        .map(|(unit, _)| unit.to_owned())
}

fn esef_statement_value(
    filing: &EsefAnnualFiling,
    concepts: &[&str],
    unit: Option<&str>,
    duration: bool,
    prior: bool,
) -> Option<f64> {
    let expected_end = filing.report_period_end;
    for concept in concepts {
        let mut values = filing
            .facts
            .iter()
            .filter(|fact| fact.concept == *concept)
            .filter(|fact| unit.is_none_or(|unit| fact.unit == unit))
            .filter(|fact| fact.period_start.is_some() == duration)
            .filter(|fact| {
                let lag = (expected_end - fact.period_end).whole_days();
                if prior {
                    (300..=450).contains(&lag)
                } else {
                    lag.abs() <= 7
                }
            })
            .filter(|fact| {
                fact.period_start.is_none_or(|start| {
                    let days = (fact.period_end - start).whole_days() + 1;
                    (250..=450).contains(&days)
                })
            })
            .map(|fact| fact.value)
            .collect::<Vec<_>>();
        values.sort_by(f64::total_cmp);
        values.dedup_by(|left, right| left.to_bits() == right.to_bits());
        if values.len() == 1 {
            return values.pop();
        }
    }
    None
}

pub fn load_stockholm(root: &Path) -> Result<(DatasetManifest, Vec<InstrumentHistory>), String> {
    let manifest: DatasetManifest = read_json(&root.join("manifest.json"))?;
    let universe: Vec<Instrument> = read_json(&root.join("universe.json"))?;
    let mut histories = Vec::new();
    for instrument in universe {
        let path = root
            .join("bars")
            .join(format!("{}.json", instrument.orderbook_id));
        if path.exists() {
            histories.push(read_json(&path)?);
        }
    }
    Ok((manifest, histories))
}

fn yahoo_symbol(symbol: &str) -> String {
    format!("{}.ST", symbol.trim().replace([' ', '/'], "-"))
}

#[derive(Debug)]
struct ParsedNasdaqMarketHistory {
    source_rows: usize,
    rejected_rows: usize,
    bars: Vec<NasdaqDailyMarketBar>,
}

fn parse_nasdaq_market_history(
    instrument: &Instrument,
    bytes: &[u8],
    start: Date,
    end: Date,
) -> Result<ParsedNasdaqMarketHistory, String> {
    let response: NasdaqMarketHistoryResponse = serde_json::from_slice(bytes).map_err(|error| {
        format!(
            "Nasdaq {} market-history response: {error}",
            instrument.orderbook_id
        )
    })?;
    let data = response.data.ok_or_else(|| {
        format!(
            "Nasdaq {} returned no market-history data: {}",
            instrument.orderbook_id,
            response
                .messages
                .map(|value| value.to_string())
                .unwrap_or_else(|| "no provider message".into())
        )
    })?;
    if data.chart_data.orderbook_id != instrument.orderbook_id
        || data.chart_data.asset_class != "SHARES"
        || data.chart_data.isin != instrument.isin
        || data.chart_data.symbol != instrument.symbol
    {
        return Err(format!(
            "Nasdaq {} identity mismatch: response id={} asset={} isin={} symbol={}",
            instrument.orderbook_id,
            data.chart_data.orderbook_id,
            data.chart_data.asset_class,
            data.chart_data.isin,
            data.chart_data.symbol
        ));
    }

    let source_rows = data.charts.rows.len();
    let mut rejected_rows = 0_usize;
    let mut bars = Vec::with_capacity(source_rows);
    for row in data.charts.rows {
        let date = parse_iso_date_prefix(&row.date_time)?;
        if date < start || date > end {
            continue;
        }
        let values = (
            nasdaq_optional_number(&row.open, "open", date)?,
            nasdaq_optional_number(&row.high, "high", date)?,
            nasdaq_optional_number(&row.low, "low", date)?,
            nasdaq_optional_number(&row.close, "close", date)?,
        );
        let (Some(open), Some(high), Some(low), Some(close)) = values else {
            rejected_rows += 1;
            continue;
        };
        if [open, high, low, close].iter().any(|value| *value <= 0.0)
            || high < low
            || open > high
            || open < low
            || close > high
            || close < low
        {
            rejected_rows += 1;
            continue;
        }
        let mut bid = nasdaq_optional_number(&row.bid, "bid", date)?
            .filter(|value| value.is_finite() && *value > 0.0);
        let mut ask = nasdaq_optional_number(&row.ask, "ask", date)?
            .filter(|value| value.is_finite() && *value > 0.0);
        if bid.zip(ask).is_some_and(|(bid, ask)| bid > ask) {
            bid = None;
            ask = None;
        }
        let average = nasdaq_optional_number(&row.average, "average", date)?
            .filter(|value| value.is_finite() && *value > 0.0);
        let total_volume =
            nasdaq_optional_number(&row.total_volume, "totalVolume", date)?.unwrap_or(0.0);
        let turnover_sek = nasdaq_optional_number(&row.turnover, "turnover", date)?.unwrap_or(0.0);
        if total_volume < 0.0 || turnover_sek < 0.0 {
            rejected_rows += 1;
            continue;
        }
        let trades = nasdaq_optional_integer(&row.trades, "trades", date)?;
        bars.push(NasdaqDailyMarketBar {
            date,
            bid,
            ask,
            open,
            high,
            low,
            close,
            average,
            total_volume,
            turnover_sek,
            trades,
        });
    }
    bars.sort_by_key(|bar| bar.date);
    bars.dedup_by_key(|bar| bar.date);
    if bars.len() < 30 {
        return Err(format!(
            "Nasdaq {} returned only {} valid daily market bars",
            instrument.orderbook_id,
            bars.len()
        ));
    }
    Ok(ParsedNasdaqMarketHistory {
        source_rows,
        rejected_rows,
        bars,
    })
}

fn nasdaq_optional_number(value: &str, field: &str, date: Date) -> Result<Option<f64>, String> {
    let normalized = value.trim().replace([',', '\u{a0}', ' '], "");
    if normalized.is_empty() {
        return Ok(None);
    }
    let parsed = normalized
        .parse::<f64>()
        .map_err(|error| format!("invalid Nasdaq {field} {value:?} on {date}: {error}"))?;
    if !parsed.is_finite() {
        return Err(format!("non-finite Nasdaq {field} on {date}"));
    }
    Ok(Some(parsed))
}

fn nasdaq_optional_integer(value: &str, field: &str, date: Date) -> Result<Option<u64>, String> {
    let normalized = value.trim().replace([',', '\u{a0}', ' '], "");
    if normalized.is_empty() {
        return Ok(None);
    }
    normalized
        .parse::<u64>()
        .map(Some)
        .map_err(|error| format!("invalid Nasdaq {field} {value:?} on {date}: {error}"))
}

fn unix_midnight(date: Date) -> Result<i64, String> {
    Ok(PrimitiveDateTime::new(date, Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp())
}

fn valid_positive(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > 0.0)
}

fn index_date(value: &str) -> Result<Date, String> {
    let milliseconds = value
        .strip_prefix("/Date(")
        .and_then(|value| value.strip_suffix(")/"))
        .ok_or_else(|| format!("invalid Nasdaq index timestamp {value:?}"))?
        .parse::<i64>()
        .map_err(|error| format!("invalid Nasdaq index timestamp {value:?}: {error}"))?;
    OffsetDateTime::from_unix_timestamp(milliseconds / 1_000)
        .map(|value| value.date())
        .map_err(|error| error.to_string())
}

fn parse_fi_historical(bytes: &[u8]) -> Result<Vec<FiHistoricalNetShortPosition>, String> {
    let rows = workbook_rows(bytes)?;
    let header = rows
        .iter()
        .position(|row| cell_text(row.get(2)) == "ISIN")
        .ok_or("FI historical workbook has no recognized header")?;
    let mut positions = Vec::new();
    for (offset, row) in rows.iter().enumerate().skip(header + 1) {
        let holder = cell_text(row.first());
        let issuer = cell_text(row.get(1));
        let isin = cell_text(row.get(2));
        if holder.is_empty() && issuer.is_empty() && isin.is_empty() {
            continue;
        }
        if !valid_isin(&isin) {
            return Err(format!(
                "FI historical row {} has invalid ISIN {isin:?}",
                offset + 1
            ));
        }
        let raw_percent = cell_text(row.get(3));
        let below_half_percent = raw_percent.trim_start().starts_with('<');
        let position_percent =
            if below_half_percent {
                None
            } else {
                Some(parse_percent(&raw_percent).map_err(|error| {
                    format!("FI historical row {} position: {error}", offset + 1)
                })?)
            };
        let position_date = parse_fi_date(&cell_text(row.get(4)))
            .map_err(|error| format!("FI historical row {} position date: {error}", offset + 1))?;
        positions.push(FiHistoricalNetShortPosition {
            holder,
            issuer,
            isin,
            position_percent,
            below_half_percent,
            position_date,
            comment: nonempty(cell_text(row.get(5))),
        });
    }
    positions.sort_by(|a, b| {
        a.position_date
            .cmp(&b.position_date)
            .then_with(|| a.isin.cmp(&b.isin))
            .then_with(|| a.holder.cmp(&b.holder))
    });
    Ok(positions)
}

fn parse_fi_aggregate(bytes: &[u8]) -> Result<Vec<FiAggregateNetShortPosition>, String> {
    let rows = workbook_rows(bytes)?;
    let header = rows
        .iter()
        .position(|row| cell_text(row.get(1)) == "LEI")
        .ok_or("FI aggregate workbook has no recognized header")?;
    let mut positions = Vec::new();
    for (offset, row) in rows.iter().enumerate().skip(header + 1) {
        let issuer = cell_text(row.first());
        let lei = cell_text(row.get(1));
        if issuer.is_empty() && lei.is_empty() {
            continue;
        }
        let position_percent = parse_percent(&cell_text(row.get(2)))
            .map_err(|error| format!("FI aggregate row {} position: {error}", offset + 1))?;
        let latest_position_date = parse_fi_date(&cell_text(row.get(3)))
            .map_err(|error| format!("FI aggregate row {} position date: {error}", offset + 1))?;
        positions.push(FiAggregateNetShortPosition {
            issuer,
            lei,
            position_percent,
            latest_position_date,
        });
    }
    positions.sort_by(|a, b| a.issuer.cmp(&b.issuer));
    Ok(positions)
}

fn workbook_rows(bytes: &[u8]) -> Result<Vec<Vec<Data>>, String> {
    let mut workbook = open_workbook_auto_from_rs(Cursor::new(bytes.to_vec()))
        .map_err(|error| format!("cannot open FI workbook: {error}"))?;
    let sheet = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or("FI workbook contains no worksheet")?;
    let range = workbook
        .worksheet_range(&sheet)
        .map_err(|error| format!("cannot read FI worksheet {sheet:?}: {error}"))?;
    Ok(range.rows().map(|row| row.to_vec()).collect())
}

fn cell_text(cell: Option<&Data>) -> String {
    match cell {
        None | Some(Data::Empty) => String::new(),
        Some(Data::String(value))
        | Some(Data::DateTimeIso(value))
        | Some(Data::DurationIso(value)) => value.trim().to_owned(),
        Some(Data::Float(value)) => value.to_string(),
        Some(Data::Int(value)) => value.to_string(),
        Some(Data::Bool(value)) => value.to_string(),
        Some(value) => value.to_string().trim().to_owned(),
    }
}

fn parse_percent(value: &str) -> Result<f64, String> {
    let parsed = value
        .trim()
        .trim_end_matches('%')
        .trim()
        .replace(',', ".")
        .parse::<f64>()
        .map_err(|error| format!("invalid percentage {value:?}: {error}"))?;
    if !parsed.is_finite() || !(0.0..=100.0).contains(&parsed) {
        return Err(format!("percentage outside [0, 100]: {parsed}"));
    }
    Ok(parsed)
}

fn parse_fi_date(value: &str) -> Result<Date, String> {
    let date = value
        .trim()
        .get(..10)
        .ok_or_else(|| format!("invalid date {value:?}"))?;
    let format = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(date, format).map_err(|error| format!("invalid date {value:?}: {error}"))
}

fn valid_isin(value: &str) -> bool {
    if value.len() != 12
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return false;
    }
    let mut digits = Vec::with_capacity(24);
    for byte in value.bytes() {
        if byte.is_ascii_digit() {
            digits.push(byte - b'0');
        } else {
            let expanded = byte - b'A' + 10;
            digits.push(expanded / 10);
            digits.push(expanded % 10);
        }
    }
    let sum = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(index, digit)| {
            let value = if index % 2 == 1 { digit * 2 } else { *digit };
            value / 10 + value % 10
        })
        .sum::<u8>();
    sum % 10 == 0
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn skv_links(html: &str, companies: bool) -> Result<std::collections::BTreeSet<String>, String> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href]").map_err(|error| error.to_string())?;
    let mut links = std::collections::BTreeSet::new();
    for anchor in document.select(&selector) {
        let Some(href) = anchor.value().attr("href") else {
            continue;
        };
        let path = href.split(['?', '#']).next().unwrap_or(href);
        let Some(remainder) = path.strip_prefix("/privat/skatter/vardepapper/aktiehistorik/")
        else {
            continue;
        };
        if !path.ends_with(".html") || remainder.starts_with("beskrivning") {
            continue;
        }
        let is_company = remainder.contains('/');
        if is_company == companies {
            links.insert(format!("{SKV_ORIGIN}{path}"));
        }
    }
    Ok(links)
}

fn skv_companies(html: &str) -> Result<Vec<SkvEquityHistoryCompany>, String> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href]").map_err(|error| error.to_string())?;
    let mut companies = BTreeMap::new();
    for anchor in document.select(&selector) {
        let Some(href) = anchor.value().attr("href") else {
            continue;
        };
        let path = href.split(['?', '#']).next().unwrap_or(href);
        let Some(remainder) = path.strip_prefix("/privat/skatter/vardepapper/aktiehistorik/")
        else {
            continue;
        };
        if !remainder.contains('/') || !path.ends_with(".html") {
            continue;
        }
        let name = anchor.text().collect::<Vec<_>>().join(" ");
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() {
            continue;
        }
        let url = format!("{SKV_ORIGIN}{path}");
        companies.insert(url.clone(), SkvEquityHistoryCompany { name, url });
    }
    Ok(companies.into_values().collect())
}

fn skv_cache_name(url: &str) -> Result<String, String> {
    let name = url
        .rsplit('/')
        .next()
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        })
        .ok_or_else(|| format!("unsafe Skatteverket company URL {url:?}"))?;
    Ok(name.into())
}

fn skv_listing_rows(
    company: &SkvEquityHistoryCompany,
    html: &str,
) -> Result<Vec<SkvListingHistoryRow>, String> {
    let document = Html::parse_document(html);
    let table_selector = Selector::parse("table.sv-aktiehistorik").map_err(|e| e.to_string())?;
    let caption_selector = Selector::parse("caption").map_err(|e| e.to_string())?;
    let row_selector = Selector::parse("tbody tr").map_err(|e| e.to_string())?;
    let cell_selector = Selector::parse("td").map_err(|e| e.to_string())?;
    let Some(table) = document.select(&table_selector).find(|table| {
        normalized_text(table.select(&caption_selector).next())
            .to_lowercase()
            .contains("namnändringar och notering på lista")
    }) else {
        // Selected foreign/unlisted reference pages may legitimately contain
        // tax events but no listing-history table. The raw page is still a
        // successful archive and contributes no listing rows.
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    for row in table.select(&row_selector) {
        let cells = row.select(&cell_selector).collect::<Vec<_>>();
        if cells.len() < 2 {
            continue;
        }
        let year_text = normalized_text(Some(cells[0]));
        let comment = normalized_text(Some(cells[1]));
        if comment.is_empty() {
            continue;
        }
        let year = year_text
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|year| (1800..=2200).contains(year));
        rows.push(SkvListingHistoryRow {
            company_name: company.name.clone(),
            source_url: company.url.clone(),
            year,
            effective_date: skv_effective_date(year, &comment),
            event_kind: skv_event_kind(&comment),
            market_hint: skv_market_hint(&comment),
            comment,
        });
    }
    Ok(rows)
}

fn skv_effective_date(year: Option<i32>, comment: &str) -> Option<Date> {
    let year = year?;
    let months = [
        ("januari", time::Month::January),
        ("februari", time::Month::February),
        ("mars", time::Month::March),
        ("april", time::Month::April),
        ("maj", time::Month::May),
        ("juni", time::Month::June),
        ("juli", time::Month::July),
        ("augusti", time::Month::August),
        ("september", time::Month::September),
        ("oktober", time::Month::October),
        ("november", time::Month::November),
        ("december", time::Month::December),
    ];
    let normalized = comment
        .to_lowercase()
        .replace(|character: char| !character.is_alphanumeric(), " ");
    let words = normalized.split_whitespace().collect::<Vec<_>>();
    for (index, word) in words.iter().enumerate() {
        let Some((_, month)) = months.iter().find(|(name, _)| name == word) else {
            continue;
        };
        let day = index
            .checked_sub(1)
            .and_then(|index| words[index].parse::<u8>().ok())?;
        if let Ok(date) = Date::from_calendar_date(year, *month, day) {
            return Some(date);
        }
    }
    None
}

fn normalized_text(element: Option<scraper::ElementRef<'_>>) -> String {
    element
        .map(|element| element.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn skv_event_kind(comment: &str) -> SkvListingEventKind {
    let lower = comment.to_lowercase();
    if lower.contains("avnoterad") || lower.contains("avnotering") {
        SkvListingEventKind::Delisting
    } else if lower.contains("ny notering") || lower.contains("nyintroduktion") {
        SkvListingEventKind::Listing
    } else if lower.contains("listbyte")
        || lower.contains("byter lista")
        || lower.contains("flyttad till")
        || lower.contains("flyttar till")
    {
        SkvListingEventKind::ListChange
    } else if lower.contains("är noterad") || lower.contains("handlas på") {
        SkvListingEventKind::Status
    } else {
        SkvListingEventKind::Other
    }
}

fn skv_market_hint(comment: &str) -> SkvMarketHint {
    let lower = comment.to_lowercase();
    if [
        "nasdaq stockholm",
        "nordiska listan",
        "o-listan",
        "a-listan",
        "stockholmsbörsen",
        "stockholms fondbörs",
    ]
    .iter()
    .any(|term| lower.contains(term))
    {
        SkvMarketHint::StockholmMainMarket
    } else if [
        "spotlight",
        "aktietorget",
        "ngm",
        "nordic sme",
        "pepmarket",
        "mangoldlistan",
    ]
    .iter()
    .any(|term| lower.contains(term))
    {
        SkvMarketHint::OtherSwedishVenue
    } else if lower.contains("first north") {
        SkvMarketHint::FirstNorth
    } else {
        SkvMarketHint::Unknown
    }
}

fn fi_query_date(date: Date) -> String {
    format!(
        "{:02}/{:02}/{:04}",
        date.day(),
        u8::from(date.month()),
        date.year()
    )
}

fn news_boundary_milliseconds(date: Date) -> Result<i128, String> {
    let time = Time::from_hms(23, 59, 59).map_err(|error| error.to_string())?;
    Ok(i128::from(
        PrimitiveDateTime::new(date, time)
            .assume_utc()
            .unix_timestamp(),
    ) * 1_000)
}

fn parse_nasdaq_news_jsonp(bytes: &[u8]) -> Result<NasdaqCompanyNewsResponse, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let start = text
        .find('{')
        .ok_or("Nasdaq company-news JSONP lacks an object")?;
    let end = text
        .rfind('}')
        .ok_or("Nasdaq company-news JSONP lacks a closing object")?;
    serde_json::from_str(&text[start..=end]).map_err(|error| error.to_string())
}

fn parse_nasdaq_publication_date(value: &str) -> Result<Date, String> {
    let date = value
        .get(..10)
        .ok_or_else(|| format!("invalid Nasdaq publication timestamp {value:?}"))?;
    let format = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(date, format)
        .map_err(|error| format!("invalid Nasdaq publication timestamp {value:?}: {error}"))
}

fn classify_nasdaq_equity_notice(headline: &str) -> NasdaqEquityNoticeKind {
    let lower = headline.to_ascii_lowercase();
    if lower.contains("delisting")
        || lower.contains("de-listing")
        || lower.contains("removal from nasdaq")
    {
        NasdaqEquityNoticeKind::Delisting
    } else if lower.contains("change of market segment")
        || lower.contains("segment change")
        || lower.contains("transfer to nasdaq stockholm")
    {
        NasdaqEquityNoticeKind::SegmentChange
    } else if lower.contains("change of company name")
        || lower.contains("change of name")
        || lower.contains("change of short name")
        || lower.contains("change of ticker")
        || lower.contains("change of isin")
        || lower.contains("new company name")
    {
        NasdaqEquityNoticeKind::IdentityChange
    } else if lower.contains("lift of suspension")
        || lower.contains("resumption of trading")
        || lower.contains("trading resumes")
    {
        NasdaqEquityNoticeKind::Resumption
    } else if lower.contains("suspension")
        || lower.contains("suspended")
        || lower.contains("trading halt")
    {
        NasdaqEquityNoticeKind::Suspension
    } else if lower.contains("listing of")
        || lower.contains("admission to trading")
        || lower.contains("first day of trading")
        || lower.contains("new share for trading")
    {
        NasdaqEquityNoticeKind::Listing
    } else {
        NasdaqEquityNoticeKind::Other
    }
}

fn parse_nasdaq_equity_notice(
    item: &NasdaqCompanyNewsItem,
    publication_date: Date,
    bytes: &[u8],
    raw_path: &Path,
) -> Result<NasdaqEquityNotice, String> {
    let html = std::str::from_utf8(bytes)
        .map_err(|error| format!("Nasdaq notice {} encoding: {error}", item.disclosure_id))?;
    let document = Html::parse_document(html);
    // Nasdaq's current renderer uses #view-body; older archived disclosures
    // render the same official text in pre.txtPre.
    let body_selector = Selector::parse("#view-body, pre").map_err(|error| error.to_string())?;
    let row_selector = Selector::parse("tr").map_err(|error| error.to_string())?;
    let cell_selector = Selector::parse("td").map_err(|error| error.to_string())?;
    let body = document
        .select(&body_selector)
        .next()
        .ok_or_else(|| format!("Nasdaq notice {} lacks #view-body", item.disclosure_id))?;
    let body_text = normalized_text(Some(body));
    let lower = body_text.to_ascii_lowercase();
    let mut short_names = BTreeMap::new();
    let mut isins = BTreeMap::new();
    let mut orderbook_ids = BTreeMap::new();
    let mut first_trading_date = None;
    let mut last_trading_date = None;
    for row in body.select(&row_selector) {
        let cells = row.select(&cell_selector).collect::<Vec<_>>();
        for pair in cells.chunks(2) {
            if pair.len() != 2 {
                continue;
            }
            let label = normalized_text(Some(pair[0]));
            let value = normalized_text(Some(pair[1]));
            let normalized_label = label
                .to_ascii_lowercase()
                .replace(|character: char| !character.is_ascii_alphanumeric(), "");
            if normalized_label.contains("shortname") || normalized_label.contains("shortcode") {
                if !value.is_empty() {
                    short_names.insert(value.clone(), ());
                }
            } else if normalized_label.contains("isincode") || normalized_label == "isin" {
                for token in identifier_tokens(&value) {
                    if valid_isin(&token) {
                        isins.insert(token, ());
                    }
                }
            } else if normalized_label.contains("orderbookid") {
                for token in identifier_tokens(&value) {
                    if token.bytes().all(|byte| byte.is_ascii_digit()) {
                        orderbook_ids.insert(token, ());
                    }
                }
            } else if normalized_label.contains("firstdayoftrading") {
                first_trading_date = first_trading_date.or_else(|| english_date(&value));
            } else if normalized_label.contains("lastdayoftrading") {
                last_trading_date = last_trading_date.or_else(|| english_date(&value));
            }
        }
    }
    for token in identifier_tokens(&body_text) {
        if valid_isin(&token) {
            isins.insert(token, ());
        }
    }
    first_trading_date = first_trading_date.or_else(|| {
        english_date_after_phrases(
            &lower,
            &[
                "first day of trading",
                "first day for trading",
                "trading will commence",
                "admitted to trading on",
            ],
        )
    });
    last_trading_date = last_trading_date.or_else(|| {
        english_date_after_phrases(
            &lower,
            &[
                "last day of trading",
                "last day for trading",
                "last trading day",
                "delisted on",
            ],
        )
    });
    Ok(NasdaqEquityNotice {
        disclosure_id: item.disclosure_id,
        headline: item.headline.clone(),
        message_url: item.message_url.clone(),
        published: item.published.clone(),
        publication_date,
        event_kind: classify_nasdaq_equity_notice(&item.headline),
        body_mentions_stockholm: lower.contains("nasdaq stockholm")
            || lower.contains("stockholm main market")
            || lower.contains("stockholm stock exchange"),
        short_names: short_names.into_keys().collect(),
        isins: isins.into_keys().collect(),
        orderbook_ids: orderbook_ids.into_keys().collect(),
        first_trading_date,
        last_trading_date,
        raw_message_file: raw_path.to_string_lossy().into_owned(),
    })
}

fn parse_nasdaq_financial_report_message(
    announcement: &NasdaqCompanyAnnouncement,
    bytes: &[u8],
    raw_path: &Path,
) -> Result<NasdaqFinancialReportMessage, String> {
    let html = String::from_utf8(bytes.to_vec()).map_err(|error| {
        format!(
            "Nasdaq report message {} is not UTF-8: {error}",
            announcement.disclosure_id
        )
    })?;
    let document = Html::parse_document(&html);
    let current_body_selector = Selector::parse("#view-body").map_err(|error| error.to_string())?;
    let legacy_body_selector = Selector::parse("pre").map_err(|error| error.to_string())?;
    let body_text = document
        .select(&current_body_selector)
        .next()
        .or_else(|| document.select(&legacy_body_selector).next())
        .map(|element| element.text().collect::<Vec<_>>().join(" "))
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            format!(
                "Nasdaq report message {} has no supported body text",
                announcement.disclosure_id
            )
        })?;
    let attachment_selector =
        Selector::parse(".attachments [href]").map_err(|error| error.to_string())?;
    let mut attachments = document
        .select(&attachment_selector)
        .filter_map(|element| {
            let url = element.value().attr("href")?.trim();
            if url.is_empty() {
                return None;
            }
            let name = element
                .text()
                .collect::<Vec<_>>()
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            Some(NasdaqNewsAttachment {
                name,
                url: url.to_owned(),
            })
        })
        .collect::<Vec<_>>();
    attachments.sort_by(|a, b| a.url.cmp(&b.url).then_with(|| a.name.cmp(&b.name)));
    attachments.dedup_by(|right, left| right.url == left.url);
    Ok(NasdaqFinancialReportMessage {
        announcement: announcement.clone(),
        body_text,
        attachments,
        raw_message_file: raw_path.to_string_lossy().into_owned(),
    })
}

fn nasdaq_attachment_cache_key(url: &str) -> String {
    url.rsplit('/')
        .next()
        .filter(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| {
            // Deterministic fallback for unexpected URL shapes. The current
            // official attachment service uses opaque alphanumeric path keys.
            let mut hash = 0xcbf29ce484222325_u64;
            for byte in url.bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
            format!("url-{hash:016x}")
        })
}

fn validate_pdf_bytes(bytes: &[u8]) -> Result<(), String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("attachment is not a PDF payload".into());
    }
    let tail = &bytes[bytes.len().saturating_sub(4096)..];
    if !tail
        .windows(b"%%EOF".len())
        .any(|window| window == b"%%EOF")
    {
        return Err("attachment PDF is incomplete (missing terminal marker)".into());
    }
    Ok(())
}

fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("data");
    let temporary = path.with_extension(format!("{extension}.tmp-{}", std::process::id()));
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("{} -> {}: {error}", temporary.display(), path.display())
    })
}

fn read_bounded_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if metadata.len() > max_bytes {
        return Err(format!(
            "{} exceeds the {max_bytes}-byte ceiling",
            path.display()
        ));
    }
    std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn run_isolated_pdf_extractor(
    input: &Path,
    output: &Path,
    max_attachment_bytes: u64,
) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let status = Command::new("timeout")
        .args([
            "--signal=KILL",
            &format!("{}s", PDF_EXTRACTOR_TIMEOUT_SECONDS),
            "prlimit",
            &format!("--as={PDF_EXTRACTOR_ADDRESS_SPACE_BYTES}"),
            "--",
        ])
        .arg(executable)
        .arg("__extract-nasdaq-report-pdf")
        .arg("--input")
        .arg(input)
        .arg("--output")
        .arg(output)
        .arg("--max-bytes")
        .arg(max_attachment_bytes.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("failed to start isolated PDF extractor: {error}"))?;
    if status.success() {
        return Ok(());
    }
    match status.code() {
        Some(124 | 137) => Err(format!(
            "isolated PDF extractor exceeded its {} second or {} MiB resource limit",
            PDF_EXTRACTOR_TIMEOUT_SECONDS,
            PDF_EXTRACTOR_ADDRESS_SPACE_BYTES / 1024 / 1024
        )),
        Some(code) => Err(format!(
            "isolated PDF extractor failed with exit code {code}"
        )),
        None => Err("isolated PDF extractor was terminated by a signal".into()),
    }
}

/// Decode one already-downloaded report PDF. Production collection invokes
/// this only in a resource-limited subprocess; keeping the decoder here makes
/// PDF interpretation a shared data-source concern rather than bot logic.
pub fn extract_nasdaq_financial_report_pdf(
    input: &Path,
    output: &Path,
    max_attachment_bytes: u64,
) -> Result<usize, String> {
    let bytes = read_bounded_file(input, max_attachment_bytes)?;
    validate_pdf_bytes(&bytes)?;
    let extracted = pdf_extract::extract_text_from_mem(&bytes)
        .map_err(|error| format!("PDF text extraction: {error}"))?;
    let normalized = normalize_document_text(&extracted);
    if normalized.is_empty() {
        return Err("PDF contains no extractable text".into());
    }
    write_atomic_bytes(output, normalized.as_bytes())?;
    Ok(normalized.chars().count())
}

fn normalize_document_text(text: &str) -> String {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn identifier_tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|character: char| !character.is_ascii_alphanumeric())
                .to_ascii_uppercase()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

fn english_date_after_phrases(text: &str, phrases: &[&str]) -> Option<Date> {
    phrases.iter().find_map(|phrase| {
        let start = text.find(phrase)? + phrase.len();
        let tail = text.get(start..)?.chars().take(180).collect::<String>();
        english_date(&tail)
    })
}

fn english_date(text: &str) -> Option<Date> {
    let months = [
        ("january", time::Month::January),
        ("february", time::Month::February),
        ("march", time::Month::March),
        ("april", time::Month::April),
        ("may", time::Month::May),
        ("june", time::Month::June),
        ("july", time::Month::July),
        ("august", time::Month::August),
        ("september", time::Month::September),
        ("october", time::Month::October),
        ("november", time::Month::November),
        ("december", time::Month::December),
    ];
    let normalized = text
        .to_ascii_lowercase()
        .replace(|character: char| !character.is_ascii_alphanumeric(), " ");
    let words = normalized.split_whitespace().collect::<Vec<_>>();
    for (index, word) in words.iter().enumerate() {
        let Some((_, month)) = months.iter().find(|(name, _)| name == word) else {
            continue;
        };
        let candidates = [
            (
                index.checked_add(1).and_then(|at| words.get(at).copied()),
                index.checked_add(2).and_then(|at| words.get(at).copied()),
            ),
            (
                index.checked_sub(1).and_then(|at| words.get(at).copied()),
                index.checked_add(1).and_then(|at| words.get(at).copied()),
            ),
        ];
        for (day, year) in candidates {
            let Some(day) = day.and_then(parse_english_day) else {
                continue;
            };
            let Some(year) = year.and_then(|value| value.parse::<i32>().ok()) else {
                continue;
            };
            if (1990..=2200).contains(&year) {
                if let Ok(date) = Date::from_calendar_date(year, *month, day) {
                    return Some(date);
                }
            }
        }
    }
    None
}

fn parse_english_day(value: &str) -> Option<u8> {
    let value = value
        .strip_suffix("st")
        .or_else(|| value.strip_suffix("nd"))
        .or_else(|| value.strip_suffix("rd"))
        .or_else(|| value.strip_suffix("th"))
        .unwrap_or(value);
    value.parse().ok().filter(|day| (1..=31).contains(day))
}

fn parse_iso_date_prefix(value: &str) -> Result<Date, String> {
    let date = value
        .get(..10)
        .ok_or_else(|| format!("invalid ISO date {value:?}"))?;
    let format = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(date, format).map_err(|error| format!("invalid ISO date {value:?}: {error}"))
}

fn parse_riksbank_observations(
    bytes: &[u8],
    start: Date,
    end: Date,
) -> Result<Vec<RiksbankObservation>, String> {
    let raw: Vec<RiksbankApiObservation> = serde_json::from_slice(bytes)
        .map_err(|error| format!("Riksbank observation response: {error}"))?;
    let mut observations = raw
        .into_iter()
        .map(|observation| {
            let date = parse_iso_date_prefix(&observation.date)?;
            if !observation.value.is_finite() {
                return Err(format!("Riksbank returned a non-finite value on {date}"));
            }
            Ok(RiksbankObservation {
                date,
                value: observation.value,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    observations.retain(|observation| observation.date >= start && observation.date <= end);
    observations.sort_by_key(|observation| observation.date);
    for pair in observations.windows(2) {
        if pair[0].date == pair[1].date {
            return Err(format!(
                "Riksbank response contains duplicate date {}",
                pair[0].date
            ));
        }
    }
    Ok(observations)
}

fn parse_esef_ifrs_facts(
    bytes: &[u8],
    report_period_end: Date,
) -> Result<Vec<EsefIfrsFact>, String> {
    let document: XbrlJsonDocument =
        serde_json::from_slice(bytes).map_err(|error| format!("xBRL-JSON: {error}"))?;
    let earliest_period_end = report_period_end - time::Duration::days(1_100);
    let latest_period_end = report_period_end + time::Duration::days(7);
    let mut facts = Vec::new();
    for fact in document.facts.into_values() {
        // A primary-statement total has only these four OIM dimensions.
        // Segment, equity-component and other breakdowns are deliberately
        // excluded rather than accidentally summed or selected.
        if fact.dimensions.len() != 4
            || !fact.dimensions.contains_key("entity")
            || !fact.dimensions.contains_key("period")
            || !fact.dimensions.contains_key("unit")
        {
            continue;
        }
        let Some(concept) = fact
            .dimensions
            .get("concept")
            .and_then(|value| value.strip_prefix("ifrs-full:"))
        else {
            continue;
        };
        let Some(period) = fact.dimensions.get("period") else {
            continue;
        };
        let Some((period_start, period_end)) = parse_xbrl_period(period)? else {
            continue;
        };
        if period_end < earliest_period_end || period_end > latest_period_end {
            continue;
        }
        let Some(unit) = fact.dimensions.get("unit") else {
            continue;
        };
        let value = match fact.value {
            None => None,
            Some(serde_json::Value::String(value)) => value.parse::<f64>().ok(),
            Some(serde_json::Value::Number(value)) => value.as_f64(),
            _ => None,
        };
        let Some(value) = value.filter(|value| value.is_finite() && value.abs() <= 1e100) else {
            continue;
        };
        facts.push(EsefIfrsFact {
            concept: concept.into(),
            period_start,
            period_end,
            unit: unit.clone(),
            value,
        });
    }
    facts.sort_by(|left, right| {
        left.concept
            .cmp(&right.concept)
            .then_with(|| left.period_start.cmp(&right.period_start))
            .then_with(|| left.period_end.cmp(&right.period_end))
            .then_with(|| left.unit.cmp(&right.unit))
            .then_with(|| left.value.total_cmp(&right.value))
    });
    facts.dedup_by(|left, right| {
        left.concept == right.concept
            && left.period_start == right.period_start
            && left.period_end == right.period_end
            && left.unit == right.unit
            && left.value.to_bits() == right.value.to_bits()
    });
    Ok(facts)
}

fn parse_xbrl_period(value: &str) -> Result<Option<(Option<Date>, Date)>, String> {
    if value == "forever" {
        return Ok(None);
    }
    let parts = value.split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        [instant] => {
            let exclusive = parse_iso_date_prefix(instant)?;
            let end = exclusive
                .previous_day()
                .ok_or_else(|| format!("xBRL period underflow {value:?}"))?;
            Ok(Some((None, end)))
        }
        [start, end] => {
            let start = parse_iso_date_prefix(start)?;
            let exclusive_end = parse_iso_date_prefix(end)?;
            let end = exclusive_end
                .previous_day()
                .ok_or_else(|| format!("xBRL period underflow {value:?}"))?;
            if end < start {
                return Ok(None);
            }
            Ok(Some((Some(start), end)))
        }
        _ => Ok(None),
    }
}

fn parse_fi_pdmr_csv(bytes: &[u8]) -> Result<Vec<FiPdmrTransaction>, String> {
    let (decoded, _, had_errors) = encoding_rs::UTF_16LE.decode(bytes);
    if had_errors || !decoded.starts_with("Publication date;") {
        return Err("export is not the expected UTF-16LE FI PDMR CSV".into());
    }
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .flexible(true)
        .from_reader(decoded.as_bytes());
    let headers = reader.headers().map_err(|error| error.to_string())?.clone();
    let index = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .ok_or_else(|| format!("FI PDMR CSV lacks header {name:?}"))
    };
    let fields = FiPdmrFields {
        publication: index("Publication date")?,
        issuer: index("Issuer")?,
        lei: index("LEI-code")?,
        notifier: index("Notifier")?,
        pdmr: index("Person discharging managerial responsibilities")?,
        position: index("Position")?,
        closely_associated: index("Closely associated")?,
        amendment: index("Amendment")?,
        amendment_details: index("Details of amendment")?,
        initial_notification: index("Initial notification")?,
        option_programme: index("Linked to share option programme")?,
        nature: index("Nature of transaction")?,
        instrument_type: index("Intrument type")?,
        instrument_name: index("Instrument name")?,
        isin: index("ISIN")?,
        transaction_date: index("Transaction date")?,
        volume: index("Volume")?,
        unit: index("Unit")?,
        price: index("Price")?,
        currency: index("Currency")?,
        venue: index("Trading venue")?,
        status: index("Status")?,
    };
    reader
        .records()
        .enumerate()
        .map(|(offset, record)| {
            let record = record.map_err(|error| error.to_string())?;
            fi_pdmr_record(&record, &fields).map_err(|error| format!("row {}: {error}", offset + 2))
        })
        .collect()
}

struct FiPdmrFields {
    publication: usize,
    issuer: usize,
    lei: usize,
    notifier: usize,
    pdmr: usize,
    position: usize,
    closely_associated: usize,
    amendment: usize,
    amendment_details: usize,
    initial_notification: usize,
    option_programme: usize,
    nature: usize,
    instrument_type: usize,
    instrument_name: usize,
    isin: usize,
    transaction_date: usize,
    volume: usize,
    unit: usize,
    price: usize,
    currency: usize,
    venue: usize,
    status: usize,
}

fn fi_pdmr_record(
    record: &csv::StringRecord,
    fields: &FiPdmrFields,
) -> Result<FiPdmrTransaction, String> {
    let get = |index| record.get(index).unwrap_or("").trim();
    let (publication_date, publication_time) = fi_datetime(get(fields.publication))?;
    let (transaction_date, _) = fi_datetime(get(fields.transaction_date))?;
    Ok(FiPdmrTransaction {
        publication_date,
        publication_time,
        issuer: get(fields.issuer).into(),
        lei: get(fields.lei).into(),
        notifier: get(fields.notifier).into(),
        pdmr: get(fields.pdmr).into(),
        position: get(fields.position).into(),
        closely_associated: fi_yes(get(fields.closely_associated)),
        amendment: fi_yes(get(fields.amendment)),
        amendment_details: nonempty(get(fields.amendment_details).into()),
        initial_notification: fi_yes(get(fields.initial_notification)),
        linked_to_share_option_programme: fi_yes(get(fields.option_programme)),
        nature: get(fields.nature).into(),
        instrument_type: get(fields.instrument_type).into(),
        instrument_name: get(fields.instrument_name).into(),
        isin: nonempty(get(fields.isin).into()),
        transaction_date,
        volume: fi_optional_number(get(fields.volume))?,
        unit: get(fields.unit).into(),
        price: fi_optional_number(get(fields.price))?,
        currency: get(fields.currency).into(),
        trading_venue: get(fields.venue).into(),
        status: get(fields.status).into(),
    })
}

fn fi_datetime(value: &str) -> Result<(Date, String), String> {
    let (date, time) = value
        .split_once(' ')
        .ok_or_else(|| format!("invalid FI date-time {value:?}"))?;
    let mut parts = date.split('/');
    let day = parts.next().and_then(|value| value.parse().ok());
    let month = parts.next().and_then(|value| value.parse::<u8>().ok());
    let year = parts.next().and_then(|value| value.parse().ok());
    let (Some(day), Some(month), Some(year), None) = (day, month, year, parts.next()) else {
        return Err(format!("invalid FI date-time {value:?}"));
    };
    let month = time::Month::try_from(month).map_err(|error| error.to_string())?;
    let date = Date::from_calendar_date(year, month, day)
        .map_err(|error| format!("invalid FI date-time {value:?}: {error}"))?;
    Ok((date, time.into()))
}

fn fi_optional_number(value: &str) -> Result<Option<f64>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    let value = value.replace(['\u{a0}', ' '], "").replace(',', ".");
    let parsed = value
        .parse::<f64>()
        .map_err(|error| format!("invalid FI number {value:?}: {error}"))?;
    if !parsed.is_finite() {
        return Err("non-finite FI number".into());
    }
    Ok(Some(parsed))
}

fn fi_yes(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "yes" | "ja")
}

fn fi_pdmr_key(transaction: &FiPdmrTransaction) -> String {
    serde_json::to_string(transaction).expect("serializable FI PDMR transaction")
}

fn parse_eodhd_symbols(bytes: &[u8]) -> Result<Vec<EodhdDelistedSymbol>, String> {
    let rows: Vec<EodhdSymbolResponse> =
        serde_json::from_slice(bytes).map_err(|error| format!("EODHD symbol response: {error}"))?;
    let mut symbols = rows
        .into_iter()
        .filter_map(|row| {
            let isin = row.isin?.trim().to_ascii_uppercase();
            (valid_isin(&isin)
                && row.security_type.eq_ignore_ascii_case("Common Stock")
                && row.exchange.eq_ignore_ascii_case("ST"))
            .then_some(EodhdDelistedSymbol {
                code: row.code,
                name: row.name,
                exchange: row.exchange,
                currency: row.currency,
                security_type: row.security_type,
                isin,
            })
        })
        .collect::<Vec<_>>();
    symbols.sort_by(|a, b| a.isin.cmp(&b.isin).then_with(|| a.code.cmp(&b.code)));
    symbols.dedup_by(|right, left| right.isin == left.isin && right.code == left.code);
    Ok(symbols)
}

fn preferred_eodhd_symbols(
    target_isins: &BTreeSet<String>,
    active: Vec<EodhdDelistedSymbol>,
    inactive: Vec<EodhdDelistedSymbol>,
) -> Vec<EodhdDelistedSymbol> {
    let mut preferred = BTreeMap::<String, EodhdDelistedSymbol>::new();
    for symbol in inactive {
        if target_isins.contains(&symbol.isin) {
            preferred.entry(symbol.isin.clone()).or_insert(symbol);
        }
    }
    for symbol in active {
        if target_isins.contains(&symbol.isin) {
            preferred.insert(symbol.isin.clone(), symbol);
        }
    }
    preferred.into_values().collect()
}

fn parse_eodhd_bars(bytes: &[u8], start: Date, end: Date) -> Result<Vec<EodhdDailyBar>, String> {
    let rows: Vec<EodhdBarResponse> =
        serde_json::from_slice(bytes).map_err(|error| format!("EODHD EOD response: {error}"))?;
    let mut bars = rows
        .into_iter()
        .filter_map(|row| {
            let date = parse_iso_date_prefix(&row.date).ok()?;
            let values = [
                row.open,
                row.high,
                row.low,
                row.close,
                row.adjusted_close,
                row.volume,
            ];
            (date >= start
                && date <= end
                && values.iter().all(|value| value.is_finite())
                && row.open > 0.0
                && row.high > 0.0
                && row.low > 0.0
                && row.close > 0.0
                && row.adjusted_close > 0.0
                && row.volume >= 0.0
                && row.low <= row.open.max(row.close)
                && row.high >= row.open.min(row.close))
            .then_some(EodhdDailyBar {
                date,
                open: row.open,
                high: row.high,
                low: row.low,
                close: row.close,
                adjusted_close: row.adjusted_close,
                volume: row.volume,
            })
        })
        .collect::<Vec<_>>();
    bars.sort_by_key(|bar| bar.date);
    bars.dedup_by_key(|bar| bar.date);
    Ok(bars)
}

fn parse_eodhd_quarterly_fundamentals(
    symbol: &EodhdDelistedSymbol,
    bytes: &[u8],
) -> Result<Vec<EodhdQuarterlyFundamental>, String> {
    let root: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("EODHD fundamentals response: {error}"))?;
    let response_isin = root
        .pointer("/General/ISIN")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_uppercase();
    if response_isin != symbol.isin {
        return Err(format!(
            "EODHD fundamentals ISIN mismatch for {}.ST: expected {}, got {:?}",
            symbol.code, symbol.isin, response_isin
        ));
    }
    let mut by_period = BTreeMap::<Date, (Date, AnnualFundamentals)>::new();
    for (statement, kind) in [
        ("Income_Statement", "income"),
        ("Balance_Sheet", "balance"),
        ("Cash_Flow", "cash_flow"),
    ] {
        let Some(section) = root.pointer(&format!("/Financials/{statement}")) else {
            continue;
        };
        let currency = section
            .get("currency_symbol")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| {
                value.len() == 3
                    && value
                        .chars()
                        .all(|character| character.is_ascii_alphabetic())
            })
            .map(|value| format!("iso4217:{}", value.to_ascii_uppercase()));
        let Some(rows) = section
            .get("quarterly")
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for (key, row) in rows {
            let period_end = row
                .get("date")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(key);
            let Ok(period_end) = parse_iso_date_prefix(period_end) else {
                continue;
            };
            let Some(filing_date) = row
                .get("filing_date")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| parse_iso_date_prefix(value).ok())
                .filter(|date| *date > period_end)
            else {
                // Statement values without an actual publication boundary are
                // unusable in a causal matrix and are intentionally dropped.
                continue;
            };
            let (available, values) = by_period
                .entry(period_end)
                .or_insert_with(|| (filing_date, empty_annual_fundamentals()));
            *available = (*available).max(filing_date);
            if values.reporting_currency.is_none() {
                values.reporting_currency.clone_from(&currency);
            }
            match kind {
                "income" => {
                    values.revenue = eodhd_number(row, &["totalRevenue", "revenue"]);
                    values.operating_profit = eodhd_number(row, &["operatingIncome", "ebit"]);
                    values.net_income = eodhd_number(row, &["netIncome"]);
                    values.basic_eps = eodhd_number(row, &["eps", "dilutedEPS"]);
                    values.weighted_average_shares = eodhd_number(
                        row,
                        &[
                            "weightedAverageShsOutDil",
                            "weightedAverageShsOut",
                            "weightedAverageShares",
                        ],
                    );
                }
                "balance" => {
                    values.assets = eodhd_number(row, &["totalAssets"]);
                    values.equity =
                        eodhd_number(row, &["totalStockholderEquity", "totalStockholdersEquity"]);
                    values.cash = eodhd_number(
                        row,
                        &["cash", "cashAndShortTermInvestments", "cashAndEquivalents"],
                    );
                    values.current_assets = eodhd_number(row, &["totalCurrentAssets"]);
                    values.current_liabilities = eodhd_number(row, &["totalCurrentLiabilities"]);
                }
                "cash_flow" => {
                    values.operating_cash_flow = eodhd_number(
                        row,
                        &[
                            "totalCashFromOperatingActivities",
                            "cashFromOperatingActivities",
                        ],
                    );
                }
                _ => unreachable!(),
            }
        }
    }
    let mut filings = by_period
        .into_iter()
        .filter_map(|(report_period_end, (available_date, values))| {
            (values.revenue.is_some()
                || values.net_income.is_some()
                || values.assets.is_some()
                || values.operating_cash_flow.is_some())
            .then_some(EodhdQuarterlyFundamental {
                report_period_end,
                available_date,
                filing_key: format!("{}:{report_period_end}:{available_date}", symbol.isin),
                values,
            })
        })
        .collect::<Vec<_>>();
    filings.sort_by_key(|filing| filing.report_period_end);
    let snapshots = filings.clone();
    for filing in &mut filings {
        let prior = snapshots
            .iter()
            .filter(|candidate| {
                let days = (filing.report_period_end - candidate.report_period_end).whole_days();
                (300..=430).contains(&days)
            })
            .min_by_key(|candidate| {
                ((filing.report_period_end - candidate.report_period_end).whole_days() - 365).abs()
            });
        if let Some(prior) = prior {
            filing.values.prior_revenue = prior.values.revenue;
            filing.values.prior_net_income = prior.values.net_income;
            filing.values.prior_assets = prior.values.assets;
            filing.values.prior_equity = prior.values.equity;
        }
    }
    // Availability order is what the causal feature cursor consumes. A later
    // restatement of an old period must not displace a newer report until its
    // own filing date is reached.
    filings.sort_by(|left, right| {
        left.available_date
            .cmp(&right.available_date)
            .then_with(|| left.report_period_end.cmp(&right.report_period_end))
            .then_with(|| left.filing_key.cmp(&right.filing_key))
    });
    Ok(filings)
}

fn empty_annual_fundamentals() -> AnnualFundamentals {
    AnnualFundamentals {
        reporting_currency: None,
        revenue: None,
        prior_revenue: None,
        operating_profit: None,
        net_income: None,
        prior_net_income: None,
        assets: None,
        prior_assets: None,
        equity: None,
        prior_equity: None,
        cash: None,
        operating_cash_flow: None,
        current_assets: None,
        current_liabilities: None,
        basic_eps: None,
        weighted_average_shares: None,
    }
}

fn eodhd_number(row: &serde_json::Value, names: &[&str]) -> Option<f64> {
    names.iter().find_map(|name| {
        let value = row.get(*name)?;
        let parsed = match value {
            serde_json::Value::Number(number) => number.as_f64(),
            serde_json::Value::String(value) => value.trim().parse::<f64>().ok(),
            _ => None,
        }?;
        parsed.is_finite().then_some(parsed)
    })
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
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

mod optional_date_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use time::Date;

    pub fn serialize<S: Serializer>(date: &Option<Date>, serializer: S) -> Result<S::Ok, S::Error> {
        date.map(|date| date.to_string()).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Date>, D::Error> {
        let value = Option::<String>::deserialize(deserializer)?;
        let format = time::macros::format_description!("[year]-[month]-[day]");
        value
            .map(|value| Date::parse(&value, format).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stockholm_symbols_map_to_yahoo_share_classes() {
        assert_eq!(yahoo_symbol("VOLV B"), "VOLV-B.ST");
        assert_eq!(yahoo_symbol("INVE A"), "INVE-A.ST");
    }

    #[test]
    fn parses_nasdaq_dotnet_timestamp() {
        assert_eq!(
            index_date("/Date(1786075200000)/").unwrap().to_string(),
            "2026-08-07"
        );
        assert!(index_date("2026-08-07").is_err());
    }

    #[test]
    fn parses_fi_localized_percentages_without_imputing_censored_values() {
        assert_eq!(parse_percent("0,71").unwrap(), 0.71);
        assert_eq!(parse_percent("1.25 %").unwrap(), 1.25);
        assert!(parse_percent("<0,5").is_err());
    }

    #[test]
    fn validates_fi_dates_and_isins() {
        assert_eq!(
            parse_fi_date("2026-08-10T00:00:00").unwrap().to_string(),
            "2026-08-10"
        );
        assert!(parse_fi_date("10/08/2026").is_err());
        assert!(valid_isin("SE0000115446"));
        assert!(!valid_isin("VOLV B"));
    }

    #[test]
    fn separates_skv_catalogue_pages_from_company_links() {
        let html = r#"
            <a href="/privat/skatter/vardepapper/aktiehistorik/df.4.group.html">D-F</a>
            <a href="/privat/skatter/vardepapper/aktiehistorik/a/acando.4.company.html"> Acando AB </a>
            <a href="/privat/skatter/vardepapper/aktiehistorik/beskrivningavaktiehistoriken.4.info.html">Info</a>
        "#;
        assert_eq!(skv_links(html, false).unwrap().len(), 1);
        let companies = skv_companies(html).unwrap();
        assert_eq!(companies.len(), 1);
        assert_eq!(companies[0].name, "Acando AB");
    }

    #[test]
    fn parses_skv_listing_rows_and_classifies_main_market_delisting() {
        let company = SkvEquityHistoryCompany {
            name: "A-Com AB".into(),
            url: "https://www.skatteverket.se/example.html".into(),
        };
        let html = r#"
          <table class="sv-aktiehistorik">
            <caption>Namnändringar och notering på lista</caption>
            <tbody>
              <tr><td></td><td>Aktien är avnoterad</td></tr>
              <tr><td>2013</td><td>Avnoterad från Nordiska listan 31 januari.</td></tr>
              <tr><td>1999</td><td>Ny notering på O-listan 4 november</td></tr>
            </tbody>
          </table>
        "#;
        let rows = skv_listing_rows(&company, html).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].year, Some(2013));
        assert_eq!(rows[1].effective_date.unwrap().to_string(), "2013-01-31");
        assert_eq!(rows[1].event_kind, SkvListingEventKind::Delisting);
        assert_eq!(rows[1].market_hint, SkvMarketHint::StockholmMainMarket);
        assert_eq!(rows[2].event_kind, SkvListingEventKind::Listing);
        assert_eq!(rows[2].effective_date.unwrap().to_string(), "1999-11-04");
    }

    #[test]
    fn current_main_market_spell_starts_after_an_explicit_venue_exit() {
        let row = |date: &str, event_kind, market_hint| SkvListingHistoryRow {
            company_name: "Example AB".into(),
            source_url: "fixture".into(),
            year: None,
            effective_date: Some(
                Date::parse(
                    date,
                    &time::macros::format_description!("[year]-[month]-[day]"),
                )
                .unwrap(),
            ),
            comment: "fixture".into(),
            event_kind,
            market_hint,
        };
        let dataset = SkvListingHistoryDataset {
            format_version: "fixture".into(),
            generated_at: "fixture".into(),
            source_catalogue: "fixture".into(),
            raw_cache_dir: "fixture".into(),
            pause_ms: 0,
            companies_requested: 1,
            companies_archived: 1,
            failures: BTreeMap::new(),
            limitations: Vec::new(),
            rows: vec![
                row(
                    "2010-01-01",
                    SkvListingEventKind::Listing,
                    SkvMarketHint::StockholmMainMarket,
                ),
                row(
                    "2015-01-01",
                    SkvListingEventKind::Listing,
                    SkvMarketHint::FirstNorth,
                ),
                row(
                    "2020-01-01",
                    SkvListingEventKind::Listing,
                    SkvMarketHint::StockholmMainMarket,
                ),
                row(
                    "2022-01-01",
                    SkvListingEventKind::Listing,
                    SkvMarketHint::StockholmMainMarket,
                ),
            ],
        };
        assert_eq!(
            skv_current_main_market_admission_dates(&dataset)["example"].to_string(),
            "2020-01-01"
        );
    }

    #[test]
    fn parses_fi_pdmr_utf16_export_and_keeps_publication_lag() {
        let csv = concat!(
            "Publication date;Issuer;LEI-code;Notifier;Person discharging managerial responsibilities;Position;Closely associated;Amendment;Details of amendment;Initial notification;Linked to share option programme;Nature of transaction;Intrument type;Instrument name;ISIN;Transaction date;Volume;Unit;Price;Currency;Trading venue;Status;\n",
            "05/12/2025 18:07:49;Test AB;LEI1;Reporter;Director;CEO;;Yes;Correction;;Yes;Acquisition;Share;Test B;SE0000000001;30/01/2025 00:00:00;5000.0;Quantity;12.30;SEK;NASDAQ STOCKHOLM AB;Current;\n"
        );
        let bytes = csv
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let rows = parse_fi_pdmr_csv(&bytes).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].publication_date.to_string(), "2025-12-05");
        assert_eq!(rows[0].transaction_date.to_string(), "2025-01-30");
        assert!(rows[0].amendment);
        assert_eq!(rows[0].volume, Some(5_000.0));
        assert_eq!(rows[0].price, Some(12.3));
    }

    #[test]
    fn parses_nasdaq_company_news_jsonp_and_publication_date() {
        let jsonp = br#"handleResponse({
          "results":{"item":[{
            "disclosureId":1450001,
            "categoryId":71,
            "headline":"Test AB interim report",
            "language":"en",
            "cnsCategory":"Interim report (Q1 and Q3)",
            "messageUrl":"https://view.news.eu.nasdaq.com/test",
            "published":"2025-10-24 07:00:00",
            "market":"Main Market, Stockholm",
            "company":"Test AB",
            "attachment":[]
          }]},
          "count":1
        })"#;
        let response = parse_nasdaq_news_jsonp(jsonp).unwrap();
        assert_eq!(response.count, 1);
        assert_eq!(response.results.item[0].disclosure_id, 1_450_001);
        assert_eq!(
            parse_nasdaq_publication_date(&response.results.item[0].published)
                .unwrap()
                .to_string(),
            "2025-10-24"
        );
    }

    #[test]
    fn parses_nasdaq_financial_report_body_and_attachment_metadata() {
        let announcement = NasdaqCompanyAnnouncement {
            disclosure_id: 1_450_001,
            category_id: 71,
            category: "Interim report (Q1 and Q3)".into(),
            headline: "Test AB interim report".into(),
            language: "en".into(),
            message_url: "https://view.news.eu.nasdaq.com/test".into(),
            published: "2025-10-24 07:00:00".into(),
            publication_date: Date::from_calendar_date(2025, time::Month::October, 24).unwrap(),
            market: "Main Market, Stockholm".into(),
            company: "Test AB".into(),
        };
        let html = br#"
          <div id="view-body">
            <p>Net sales amounted to SEK 414 (405) million.</p>
            <p>EBIT margin was 15.9 (15.5) percent.</p>
          </div>
          <div class="attachments">
            <nef-link href="https://attachment.news.eu.nasdaq.com/report">Report Q3.pdf</nef-link>
          </div>
        "#;
        let message = parse_nasdaq_financial_report_message(
            &announcement,
            html,
            Path::new("raw/messages/1450001.html"),
        )
        .unwrap();
        assert_eq!(
            message.body_text,
            "Net sales amounted to SEK 414 (405) million. EBIT margin was 15.9 (15.5) percent."
        );
        assert_eq!(
            message.attachments,
            [NasdaqNewsAttachment {
                name: "Report Q3.pdf".into(),
                url: "https://attachment.news.eu.nasdaq.com/report".into(),
            }]
        );
        let legacy = br#"<html><body><main><div><pre class="txtPre">Net sales were SEK 100 million.</pre></div></main></body></html>"#;
        let message = parse_nasdaq_financial_report_message(
            &announcement,
            legacy,
            Path::new("raw/messages/1450001.html"),
        )
        .unwrap();
        assert_eq!(message.body_text, "Net sales were SEK 100 million.");
    }

    #[test]
    fn attachment_cache_keys_are_safe_and_pdf_validation_is_explicit() {
        assert_eq!(
            nasdaq_attachment_cache_key(
                "https://attachment.news.eu.nasdaq.com/ad4fd6be084d7f31c4b76841d1415019b"
            ),
            "ad4fd6be084d7f31c4b76841d1415019b"
        );
        assert!(nasdaq_attachment_cache_key("https://example.test/a/b?bad=1").starts_with("url-"));
        assert!(validate_pdf_bytes(b"%PDF-1.7\nbody\n%%EOF\n").is_ok());
        assert!(validate_pdf_bytes(b"%PDF-1.7\ntruncated").is_err());
        assert!(validate_pdf_bytes(b"<html>not a pdf</html>").is_err());
        assert_eq!(
            normalize_document_text(" Net   sales  100 \n\n EBIT  10 "),
            "Net sales 100\nEBIT 10"
        );
    }

    #[test]
    fn parses_nasdaq_stockholm_delisting_identifiers_and_last_session() {
        let item = NasdaqCompanyNewsItem {
            disclosure_id: 1_196_927,
            category_id: 71,
            headline: "Delisting of Swedish Match AB from Nasdaq Stockholm (189/22)".into(),
            language: "en".into(),
            cns_category: "Equity Market information".into(),
            message_url: "https://view.news.eu.nasdaq.com/example".into(),
            published: "2022-12-13 13:59:56".into(),
            market: "NASDAQ OMX Nordic".into(),
            company: "Nasdaq Stockholm AB".into(),
        };
        let html = br#"
          <div id="view-body">
            <p>Nasdaq Stockholm has decided to delist the shares.</p>
            <table><tr><td>Short name:</td><td>SWMA</td></tr>
              <tr><td>ISIN code:</td><td>SE0015812219</td></tr>
              <tr><td>Order book ID:</td><td>361</td></tr></table>
            <p>The last day of trading will be December 30, 2022.</p>
          </div>
        "#;
        let notice = parse_nasdaq_equity_notice(
            &item,
            Date::from_calendar_date(2022, time::Month::December, 13).unwrap(),
            html,
            Path::new("raw/1196927.html"),
        )
        .unwrap();
        assert_eq!(notice.event_kind, NasdaqEquityNoticeKind::Delisting);
        assert!(notice.body_mentions_stockholm);
        assert_eq!(notice.short_names, ["SWMA"]);
        assert_eq!(notice.isins, ["SE0015812219"]);
        assert_eq!(notice.orderbook_ids, ["361"]);
        assert_eq!(notice.last_trading_date.unwrap().to_string(), "2022-12-30");
    }

    #[test]
    fn parses_official_nasdaq_market_history_and_normalizes_activity() {
        let instrument = Instrument {
            orderbook_id: "TX100".into(),
            isin: "SE0000115446".into(),
            symbol: "VOLV B".into(),
            name: "Volvo B".into(),
            currency: "SEK".into(),
            sector: "Industrials".into(),
            bucket: UniverseBucket::LargeCap,
            yahoo_symbol: "VOLV-B.ST".into(),
        };
        let bytes = br#"{
          "data": {
            "chartData": {
              "orderbookId": "TX100",
              "assetClass": "SHARES",
              "isin": "SE0000115446",
              "symbol": "VOLV B"
            },
            "charts": {"rows": [
              {"dateTime":"2026-08-11","bid":"350.30","ask":"350.40",
               "open":"354.40","high":"354.60","low":"349.80","close":"350.00",
               "average":"350.8889","totalVolume":"2,358,429",
               "turnover":"827,569,954.4","trades":"7,031"}
            ]}
          },
          "messages": null
        }"#;
        let start = Date::from_calendar_date(2026, time::Month::August, 1).unwrap();
        let end = Date::from_calendar_date(2026, time::Month::August, 12).unwrap();
        let error = parse_nasdaq_market_history(&instrument, bytes, start, end).unwrap_err();
        assert!(error.contains("only 1 valid daily market bars"));

        let response: NasdaqMarketHistoryResponse = serde_json::from_slice(bytes).unwrap();
        let row = &response.data.unwrap().charts.rows[0];
        assert_eq!(
            nasdaq_optional_number(&row.total_volume, "volume", end).unwrap(),
            Some(2_358_429.0)
        );
        assert_eq!(
            nasdaq_optional_integer(&row.trades, "trades", end).unwrap(),
            Some(7_031)
        );
    }

    #[test]
    fn joins_nasdaq_security_and_news_issuer_names_without_share_classes() {
        assert_eq!(
            stockholm_security_issuer_key("Atlas Copco B"),
            nasdaq_news_issuer_key("Atlas Copco AB (publ)")
        );
        assert_eq!(
            stockholm_security_issuer_key("Volvo B"),
            nasdaq_news_issuer_key("Volvo, AB")
        );
        assert_eq!(
            stockholm_security_issuer_key("Fastator"),
            nasdaq_news_issuer_key("AB Fastator")
        );
    }

    #[test]
    fn parses_esef_periods_as_inclusive_calendar_dates() {
        let (start, end) = parse_xbrl_period("2022-01-01T00:00:00/2023-01-01T00:00:00")
            .unwrap()
            .unwrap();
        assert_eq!(start.unwrap().to_string(), "2022-01-01");
        assert_eq!(end.to_string(), "2022-12-31");
        let (start, end) = parse_xbrl_period("2023-01-01T00:00:00").unwrap().unwrap();
        assert!(start.is_none());
        assert_eq!(end.to_string(), "2022-12-31");
        assert!(parse_xbrl_period("forever").unwrap().is_none());
    }

    #[test]
    fn esef_parser_retains_only_standard_nondimensional_numeric_facts() {
        let payload = br#"{
          "facts": {
            "good": {"value":"123.5","dimensions":{
              "concept":"ifrs-full:Revenue","entity":"scheme:LEI",
              "period":"2022-01-01T00:00:00/2023-01-01T00:00:00",
              "unit":"iso4217:SEK"}},
            "extension": {"value":"9","dimensions":{
              "concept":"issuer:AdjustedRevenue","entity":"scheme:LEI",
              "period":"2022-01-01T00:00:00/2023-01-01T00:00:00",
              "unit":"iso4217:SEK"}},
            "segment": {"value":"7","dimensions":{
              "concept":"ifrs-full:Revenue","entity":"scheme:LEI",
              "period":"2022-01-01T00:00:00/2023-01-01T00:00:00",
              "unit":"iso4217:SEK","ifrs-full:SegmentsAxis":"issuer:Sweden"}}
          }
        }"#;
        let report_end = parse_iso_date_prefix("2022-12-31").unwrap();
        let facts = parse_esef_ifrs_facts(payload, report_end).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].concept, "Revenue");
        assert_eq!(facts[0].period_end, report_end);
        assert_eq!(facts[0].value, 123.5);
    }

    #[test]
    fn normalizes_current_and_comparative_esef_statement_values() {
        let report_end = parse_iso_date_prefix("2023-12-31").unwrap();
        let fact = |concept: &str, start: Option<&str>, end: &str, value: f64| EsefIfrsFact {
            concept: concept.into(),
            period_start: start.map(|value| parse_iso_date_prefix(value).unwrap()),
            period_end: parse_iso_date_prefix(end).unwrap(),
            unit: "iso4217:SEK".into(),
            value,
        };
        let filing = EsefAnnualFiling {
            filing_id: "1".into(),
            entity_name: "Test AB".into(),
            lei: "LEI".into(),
            report_period_end: report_end,
            repository_date_added: parse_iso_date_prefix("2024-03-01").unwrap(),
            official_annual_report_date: None,
            available_date: parse_iso_date_prefix("2024-03-01").unwrap(),
            json_url: "https://example.test/report.json".into(),
            package_url: "https://example.test/report.zip".into(),
            sha256: "hash".into(),
            error_count: 0,
            warning_count: 0,
            inconsistency_count: 0,
            facts: vec![
                fact("Assets", None, "2023-12-31", 1_000.0),
                fact("Assets", None, "2022-12-31", 900.0),
                fact("Revenue", Some("2023-01-01"), "2023-12-31", 500.0),
                fact("Revenue", Some("2022-01-01"), "2022-12-31", 400.0),
            ],
        };
        let values = normalize_esef_annual_fundamentals(&filing);
        assert_eq!(values.reporting_currency.as_deref(), Some("iso4217:SEK"));
        assert_eq!(values.assets, Some(1_000.0));
        assert_eq!(values.prior_assets, Some(900.0));
        assert_eq!(values.revenue, Some(500.0));
        assert_eq!(values.prior_revenue, Some(400.0));
    }

    #[test]
    fn parses_and_orders_riksbank_interval_observations() {
        let bytes = br#"[
          {"date":"2024-01-03","value":10.2},
          {"date":"2024-01-02","value":10.1},
          {"date":"2023-12-29","value":10.0}
        ]"#;
        let values = parse_riksbank_observations(
            bytes,
            parse_iso_date_prefix("2024-01-01").unwrap(),
            parse_iso_date_prefix("2024-01-31").unwrap(),
        )
        .unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].date.to_string(), "2024-01-02");
        assert_eq!(values[1].value, 10.2);
    }

    #[test]
    fn parses_eodhd_delisted_common_stocks_and_valid_eod_rows() {
        let symbols = br#"[
          {"Code":"SWMA_old","Name":"Swedish Match AB","Country":"Sweden","Exchange":"ST","Currency":"SEK","Type":"Common Stock","Isin":"SE0015812219"},
          {"Code":"FUND","Name":"Fund","Country":"Sweden","Exchange":"ST","Currency":"SEK","Type":"Fund","Isin":"SE0000000001"}
        ]"#;
        let symbols = parse_eodhd_symbols(symbols).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].code, "SWMA_old");
        assert_eq!(symbols[0].isin, "SE0015812219");

        let bars = br#"[
          {"date":"2022-12-29","open":114.8,"high":115.0,"low":114.7,"close":114.9,"adjusted_close":114.9,"volume":1000},
          {"date":"2023-01-03","open":0,"high":0,"low":0,"close":0,"adjusted_close":0,"volume":0}
        ]"#;
        let bars = parse_eodhd_bars(
            bars,
            parse_iso_date_prefix("2022-01-01").unwrap(),
            parse_iso_date_prefix("2023-12-31").unwrap(),
        )
        .unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].date.to_string(), "2022-12-29");
        assert_eq!(bars[0].adjusted_close, 114.9);
    }

    #[test]
    fn eodhd_fundamental_symbol_selection_prefers_active_code_for_same_isin() {
        let symbol = |code: &str| EodhdDelistedSymbol {
            code: code.into(),
            name: "Test AB".into(),
            exchange: "ST".into(),
            currency: "SEK".into(),
            security_type: "Common Stock".into(),
            isin: "SE0015812219".into(),
        };
        let selected = preferred_eodhd_symbols(
            &BTreeSet::from(["SE0015812219".to_owned()]),
            vec![symbol("TEST")],
            vec![symbol("TEST_old")],
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].code, "TEST");
    }

    #[test]
    fn parses_eodhd_quarterly_fundamentals_only_at_filing_date() {
        let symbol = EodhdDelistedSymbol {
            code: "TEST".into(),
            name: "Test AB".into(),
            exchange: "ST".into(),
            currency: "SEK".into(),
            security_type: "Common Stock".into(),
            isin: "SE0015812219".into(),
        };
        let bytes = br#"{
          "General":{"ISIN":"SE0015812219"},
          "Financials":{
            "Income_Statement":{"currency_symbol":"SEK","quarterly":{
              "2023-03-31":{"date":"2023-03-31","filing_date":"2023-04-28","totalRevenue":"100","operatingIncome":"10","netIncome":"8","eps":"0.8","weightedAverageShsOut":"10"},
              "2024-03-31":{"date":"2024-03-31","filing_date":"2024-04-26","totalRevenue":"120","operatingIncome":"15","netIncome":"12","eps":"1.2","weightedAverageShsOut":"10"},
              "2024-06-30":{"date":"2024-06-30","filing_date":"0000-00-00","totalRevenue":"130"}
            }},
            "Balance_Sheet":{"currency_symbol":"SEK","quarterly":{
              "2023-03-31":{"date":"2023-03-31","filing_date":"2023-04-28","totalAssets":"200","totalStockholderEquity":"80"},
              "2024-03-31":{"date":"2024-03-31","filing_date":"2024-04-29","totalAssets":"240","totalStockholderEquity":"100","cash":"30","totalCurrentAssets":"90","totalCurrentLiabilities":"45"}
            }},
            "Cash_Flow":{"currency_symbol":"SEK","quarterly":{
              "2024-03-31":{"date":"2024-03-31","filing_date":"2024-04-26","totalCashFromOperatingActivities":"18"}
            }}
          }
        }"#;
        let filings = parse_eodhd_quarterly_fundamentals(&symbol, bytes).unwrap();
        assert_eq!(filings.len(), 2);
        let latest = filings.last().unwrap();
        assert_eq!(latest.report_period_end.to_string(), "2024-03-31");
        assert_eq!(latest.available_date.to_string(), "2024-04-29");
        assert_eq!(
            latest.values.reporting_currency.as_deref(),
            Some("iso4217:SEK")
        );
        assert_eq!(latest.values.revenue, Some(120.0));
        assert_eq!(latest.values.prior_revenue, Some(100.0));
        assert_eq!(latest.values.prior_assets, Some(200.0));
        assert_eq!(latest.values.operating_cash_flow, Some(18.0));
    }
}
