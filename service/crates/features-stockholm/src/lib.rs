//! Sole implementation of Stockholm-equity feature values used by research,
//! training-matrix generation, replay, and live inference.
//!
//! Raw OHLCV and corporate-action-adjusted closes enter this crate as owned
//! records. Broker requests do not. Python receives only the final normalised
//! values emitted here (or by a Rust portfolio matrix builder calling here).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use time::Date;

// Every stock feature set shares one candidate-building and cross-section
// path, so a change to how decision-date membership or its labels are settled
// changes all of them at once. Versions 1-16 are the contracts in force before
// decision-date membership stopped depending on future label availability;
// each advanced by the same offset of sixteen, so old version `n` is new
// version `n + 16` and no number is ever reused. The separate direction matrix
// has its own builder and keeps its own `fs-rust-stockholm-direction-*` line.
pub const FEATURE_SET_VERSION: &str = "fs-rust-stockholm-18";
pub const BASELINE_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-17";
pub const BASELINE_GLOBAL_RISK_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-30";
pub const RESIDUAL_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-19";
pub const PUBLIC_SHORT_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-20";
pub const PDMR_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-21";
pub const REPORT_EVENT_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-22";
pub const FUNDAMENTAL_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-23";
pub const QUARTERLY_FUNDAMENTAL_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-31";
pub const PDMR_MACRO_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-24";
pub const PDMR_MICROSTRUCTURE_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-25";
pub const PDMR_MICROSTRUCTURE_BORROW_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-26";
pub const PDMR_MICROSTRUCTURE_BORROW_NEWS_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-27";
pub const PDMR_MICROSTRUCTURE_BORROW_NEWS_GLOBAL_RISK_FEATURE_SET_VERSION: &str =
    "fs-rust-stockholm-32";
pub const PDMR_MICROSTRUCTURE_BORROW_NEWS_REPORT_TEXT_FEATURE_SET_VERSION: &str =
    "fs-rust-stockholm-28";
pub const PDMR_MICROSTRUCTURE_BORROW_NEWS_REPORT_ATTACHMENTS_FEATURE_SET_VERSION: &str =
    "fs-rust-stockholm-29";
pub const MARKET_TREND_VERSION: &str = "stockholm-market-trend-1";
/// Diagnostics-only: the FI historical short-register is keyed by
/// `position_date`, and late filings backfill that date, so any candidate
/// model built on it risks look-ahead. This line keeps the full
/// `PublicShortCursor` family reachable for research (e.g. quantifying the
/// look-ahead itself) without it ever appearing in a candidate contract; see
/// `FeatureSet::DiagnosticsPublicShortLookahead`. It intentionally does not
/// share the `fs-rust-stockholm-<ordinal>` candidate line or numbering.
pub const DIAGNOSTICS_PUBLIC_SHORT_LOOKAHEAD_FEATURE_SET_VERSION: &str =
    "fs-rust-stockholm-diagnostics-public-short-lookahead-1";
pub const DIRECTION_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-direction-1";
pub const DIRECTION_GLOBAL_RISK_FEATURE_SET_VERSION: &str = "fs-rust-stockholm-direction-2";
pub const DIRECTION_STOCKHOLM_CLOSE_GLOBAL_RISK_FEATURE_SET_VERSION: &str =
    "fs-rust-stockholm-direction-3";
pub const DIRECTION_INDEX_SYMBOLS: &[&str] = &[
    "OMXSGI", "OMXS30GI", "OMXSBGI", "SX10GI", "SX15GI", "SX20GI", "SX30GI", "SX50GI", "SX55GI",
    "SX60GI", "SX65GI",
];
pub const DIRECTION_SECTOR_SYMBOLS: &[&str] = &[
    "SX10GI", "SX15GI", "SX20GI", "SX30GI", "SX50GI", "SX55GI", "SX60GI", "SX65GI",
];
pub const DIRECTION_RAW_FEATURE_NAMES: &[&str] = &[
    "omxsgi_ret_5",
    "omxsgi_ret_21",
    "omxsgi_ret_63",
    "omxsgi_ret_126",
    "omxsgi_ret_252",
    "omxsgi_price_vs_ma50",
    "omxsgi_ma50_vs_ma200",
    "omxsgi_vol_20",
    "omxsgi_vol_60",
    "omxsgi_max_drawdown_126",
    "omxs30gi_excess_ret_21",
    "omxs30gi_excess_ret_63",
    "omxs30gi_excess_ret_126",
    "omxsbgi_excess_ret_21",
    "omxsbgi_excess_ret_63",
    "omxsbgi_excess_ret_126",
    "sector_breadth_positive_21",
    "sector_breadth_positive_63",
    "sector_breadth_positive_126",
    "sector_median_ret_21",
    "sector_median_ret_63",
    "sector_median_ret_126",
    "sector_dispersion_ret_21",
    "sector_dispersion_ret_63",
];
/// Per-instrument trailing features. These are calculated independently and
/// then ranked inside the point-in-time size-bucket cross-section.
pub const FEATURE_NAMES: &[&str] = &[
    "ret_1",
    "ret_5",
    "ret_21",
    "ret_63",
    "ret_126",
    "ret_252",
    "ret_21_skip_5",
    "vol_20",
    "vol_60",
    "downside_vol_60",
    "skew_60",
    "max_drawdown_126",
    "dist_high_252",
    "dist_low_252",
    "gap_1",
    "range_frac_1",
    "close_location_1",
    "median_traded_notional_20",
    "amihud_20",
    "volume_surge_20",
];

/// Global risk context from the shared CME archive. These values are common
/// to every stock on a decision date and therefore remain in level units;
/// cross-sectional ranking would erase them. Only the last completed UTC day
/// strictly before the Stockholm decision date is admissible.
pub const GLOBAL_RISK_FEATURE_NAMES: &[&str] = &[
    "es_ret_1",
    "es_ret_5",
    "es_ret_21",
    "es_ret_63",
    "es_ret_126",
    "es_vol_20",
    "es_vol_60",
    "es_max_drawdown_126",
];
pub const STOCKHOLM_CLOSE_GLOBAL_RISK_SYMBOLS: &[&str] = &["ES", "NQ", "ZN", "GC"];

/// Predeclared residual-risk and execution-context additions from the v3
/// design. Values are calculated causally from the eligible main-market
/// universe, then ranked inside the same size-bucket cross-section as v1.
pub const RESIDUAL_FEATURE_NAMES: &[&str] = &[
    "beta_252",
    "idio_vol_60",
    "log_adv_sek_20",
    "log_adv_sek_60",
    "amihud_60",
    "range_frac_14",
    "close_location_5",
    "market_resid_ret_21",
    "market_resid_ret_126",
    "sector_resid_ret_21",
    "sector_resid_ret_126",
];

/// Public-disclosure short-demand features. These describe disclosed holder
/// crowding and its changes; they are not stock-loan inventory or historical
/// locate availability. Events are admitted strictly after their position
/// date, conservatively avoiding same-day publication leakage.
pub const PUBLIC_SHORT_FEATURE_NAMES: &[&str] = &[
    "fi_public_short_percent",
    "fi_public_short_holder_count",
    "fi_public_short_change_30d",
    "fi_public_short_change_90d",
    "fi_days_since_public_short_event",
    "fi_public_short_events_30d",
];

/// Manager transaction features derived from FI's public PDMR register. Only
/// initial notifications of cash-valued share acquisitions and disposals are
/// admitted. This avoids using today's revised/cancelled status to rewrite
/// what was observable on an earlier decision date.
pub const PDMR_FEATURE_NAMES: &[&str] = &[
    "fi_pdmr_net_value_30d",
    "fi_pdmr_net_value_90d",
    "fi_pdmr_buy_value_90d",
    "fi_pdmr_sell_value_90d",
    "fi_pdmr_transactions_30d",
    "fi_pdmr_unique_buyers_90d",
    "fi_days_since_pdmr_buy",
];

/// Official Nasdaq financial-report release features. Event counts and
/// reactions deduplicate translations sharing one issuer/timestamp. A report
/// is never available on its publication date in daily research.
pub const REPORT_EVENT_FEATURE_NAMES: &[&str] = &[
    "nasdaq_days_since_financial_report",
    "nasdaq_financial_reports_30d",
    "nasdaq_financial_reports_90d",
    "nasdaq_financial_report_reaction_1",
    "nasdaq_financial_report_reaction_5",
    "nasdaq_financial_report_reaction_21",
];

/// Point-in-time annual fundamental features from standardized financial
/// statement fields. Availability is controlled by the source event; all
/// ratios and price joins are finalized here in Rust.
pub const FUNDAMENTAL_FEATURE_NAMES: &[&str] = &[
    "days_since_annual_fundamentals",
    "fundamental_equity_to_assets",
    "fundamental_cash_to_assets",
    "fundamental_cfo_to_assets",
    "fundamental_accruals_to_assets",
    "fundamental_operating_margin",
    "fundamental_net_margin",
    "fundamental_revenue_growth",
    "fundamental_net_income_change_to_assets",
    "fundamental_asset_growth",
    "fundamental_equity_growth",
    "fundamental_current_ratio",
    "fundamental_eps_yield",
    "fundamental_book_to_market",
    "fundamental_sales_to_market",
];

/// The same provider-neutral ratios as the annual contract, but the freshness
/// field explicitly identifies a quarterly, filing-date-controlled source.
/// A separate feature/version contract prevents annual ESEF and quarterly
/// licensed data from being mistaken for interchangeable model inputs.
pub const QUARTERLY_FUNDAMENTAL_FEATURE_NAMES: &[&str] = &[
    "days_since_quarterly_fundamentals",
    "fundamental_equity_to_assets",
    "fundamental_cash_to_assets",
    "fundamental_cfo_to_assets",
    "fundamental_accruals_to_assets",
    "fundamental_operating_margin",
    "fundamental_net_margin",
    "fundamental_revenue_growth",
    "fundamental_net_income_change_to_assets",
    "fundamental_asset_growth",
    "fundamental_equity_growth",
    "fundamental_current_ratio",
    "fundamental_eps_yield",
    "fundamental_book_to_market",
    "fundamental_sales_to_market",
];

pub const MACRO_FEATURE_NAMES: &[&str] = &[
    "usdsek_beta_126",
    "eursek_beta_126",
    "kix_beta_126",
    "usdsek_trend_exposure_21",
    "eursek_trend_exposure_21",
    "kix_trend_exposure_21",
    "usdsek_trend_exposure_63",
    "eursek_trend_exposure_63",
    "kix_trend_exposure_63",
];

/// Exchange-observed liquidity and closing-pressure features. The source
/// records are completed-session Nasdaq observations and are used only for a
/// next-session entry. Missing quotes remain missing rather than being
/// replaced with a zero spread.
pub const MICROSTRUCTURE_FEATURE_NAMES: &[&str] = &[
    "nasdaq_spread_bps_1",
    "nasdaq_median_spread_bps_20",
    "nasdaq_spread_ratio_20",
    "nasdaq_log_median_trades_20",
    "nasdaq_trades_surge_20",
    "nasdaq_close_vs_average_1",
    "nasdaq_log_median_trade_value_20",
];

/// Historical stock-borrow fee features. IB's FEE_RATE bars are decimal
/// annual rates (0.01 means 1% per year), not availability or a locate.
pub const BORROW_FEE_FEATURE_NAMES: &[&str] = &[
    "ib_borrow_fee_1",
    "ib_median_borrow_fee_20",
    "ib_borrow_fee_change_5",
    "ib_borrow_fee_change_20",
    "ib_max_borrow_fee_20",
];

/// Timestamped Nasdaq issuer-news features. The provider category is mapped
/// to [`CompanyNewsKind`] before entering this crate; no headline text or
/// future price response is interpreted by the training orchestrator.
pub const COMPANY_NEWS_FEATURE_NAMES: &[&str] = &[
    "nasdaq_company_news_30d",
    "nasdaq_company_news_90d",
    "nasdaq_days_since_inside_information",
    "nasdaq_inside_information_30d",
    "nasdaq_inside_information_90d",
    "nasdaq_inside_information_reaction_1",
    "nasdaq_inside_information_reaction_5",
    "nasdaq_inside_information_reaction_21",
    "nasdaq_own_shares_news_90d",
    "nasdaq_management_news_90d",
    "nasdaq_prospectus_news_90d",
    "nasdaq_major_shareholder_news_90d",
    "nasdaq_tender_offer_news_90d",
];

/// Accounting changes extracted from official issuer-authored Nasdaq report
/// bodies. Values are admitted only on later decision dates; exact bilingual
/// labels and current/comparative numeric pairs are parsed in this Rust crate.
pub const REPORT_TEXT_FEATURE_NAMES: &[&str] = &[
    "nasdaq_days_since_report_text",
    "nasdaq_report_sales_growth",
    "nasdaq_report_order_intake_growth",
    "nasdaq_report_ebit_growth",
    "nasdaq_report_operating_margin",
    "nasdaq_report_operating_margin_change",
    "nasdaq_report_eps_growth",
    "nasdaq_report_dividend_growth",
];

pub const REQUIRED_MACRO_SERIES: &[&str] = &["SEKUSDPMI", "SEKEURPMI", "SEKKIX92"];

/// Causal cross-sectional context calculated from the same decision-date
/// universe. Sector-relative values are ranked within size bucket; market
/// regime values stay in economic units so a common regime is not erased by
/// cross-sectional ranking.
pub const CONTEXT_FEATURE_NAMES: &[&str] = &[
    "sector_rel_ret_5",
    "sector_rel_ret_21",
    "sector_rel_ret_63",
    "sector_rel_ret_126",
    "sector_rel_ret_252",
    "sector_rel_vol_60",
    "sector_rel_amihud_20",
    "sector_rel_volume_surge_20",
    "market_median_ret_5",
    "market_median_ret_21",
    "market_median_ret_63",
    "market_median_ret_126",
    "market_median_ret_252",
    "market_dispersion_ret_21",
    "market_breadth_positive_21",
    "market_breadth_positive_126",
    "market_median_vol_20",
];

const SECTOR_RELATIVE_SOURCES: &[&str] = &[
    "ret_5",
    "ret_21",
    "ret_63",
    "ret_126",
    "ret_252",
    "vol_60",
    "amihud_20",
    "volume_surge_20",
];

const MARKET_MEDIAN_SOURCES: &[&str] = &["ret_5", "ret_21", "ret_63", "ret_126", "ret_252"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub instrument_id: String,
    /// Unadjusted exchange prices. These preserve economically meaningful
    /// ranges, gaps, prices, and traded notional.
    pub raw_open: f64,
    pub raw_high: f64,
    pub raw_low: f64,
    pub raw_close: f64,
    pub volume: f64,
    /// Split/dividend-adjusted close used only for return-like features.
    pub adjusted_close: f64,
}

/// Provider-neutral broad-market close used by the slow direction layer. The
/// Stockholm adapter supplies OMXSGI EOD levels; no provider type crosses this
/// feature boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub close: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacroObservation {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacroSeries {
    pub series_id: String,
    pub observations: Vec<MacroObservation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalRiskBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub close: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalRiskSeries {
    pub symbol: String,
    pub observations: Vec<GlobalRiskBar>,
}

/// Provider-neutral completed-session market microstructure observation.
/// Provider response parsing and units remain owned by `equity-data`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketMicrostructureBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub instrument_id: String,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub close: f64,
    pub average: Option<f64>,
    pub turnover_sek: f64,
    pub trades: Option<u64>,
}

/// Provider-neutral completed-session stock-borrow cost. The IB adapter owns
/// request semantics and unit decoding; this crate only sees an annual rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BorrowFeeBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub instrument_id: String,
    pub annual_rate: f64,
}

/// Provider-neutral official index session used by the trained market
/// direction layer. Features and labels both use EOD values: `start_value` is
/// the provider's start-of-day level, which is the prior session's close plus
/// any dividend adjustment rather than an opening-auction print, so it is kept
/// for provenance and never priced against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketIndexBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub start_value: f64,
    pub end_value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketIndexSeries {
    pub symbol: String,
    pub bars: Vec<MarketIndexBar>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectionTrainingRow {
    #[serde(with = "date_serde")]
    pub date: Date,
    /// OMXSGI close of the first tradable session to the close of the session
    /// the declared horizon ends on.
    pub target: f64,
    /// Direction-only secondary label finalized in Rust. Zero returns remain
    /// neutral; Python may select this value for fitting but may not derive it.
    #[serde(default)]
    pub sign_target: f64,
    pub entry_value: f64,
    pub exit_value: f64,
    pub annualised_volatility_20: f64,
    /// Final Rust-owned values in the matrix's declared feature order.
    pub features: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectionTrainingMatrix {
    pub features: Vec<String>,
    pub rows: Vec<DirectionTrainingRow>,
}

/// v4 anchors both legs on official closing levels. The retired
/// `omxsgi-forward-start-value-*-v1` versions anchored on the archive's SOD,
/// which is the prior session's close, so their labels credited the untradable
/// gap into the first held session. The prefix changes with the convention, so
/// a matrix or model carrying the old contract cannot be parsed — let alone
/// silently mixed — by anything built here.
pub fn direction_label_version(horizon_sessions: usize) -> Result<String, String> {
    if horizon_sessions == 0 {
        return Err("direction label horizon must be positive".into());
    }
    Ok(format!("omxsgi-forward-close-{horizon_sessions}-v4"))
}

pub fn direction_label_horizon(version: &str) -> Option<usize> {
    version
        .strip_prefix("omxsgi-forward-close-")?
        .strip_suffix("-v4")?
        .parse()
        .ok()
        .filter(|horizon| *horizon > 0)
}

pub fn direction_model_feature_names() -> Vec<String> {
    DIRECTION_RAW_FEATURE_NAMES
        .iter()
        .map(|name| format!("x_{name}"))
        .chain(
            DIRECTION_RAW_FEATURE_NAMES
                .iter()
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn direction_global_risk_model_feature_names() -> Vec<String> {
    direction_model_feature_names()
        .into_iter()
        .chain(
            GLOBAL_RISK_FEATURE_NAMES
                .iter()
                .map(|name| format!("g_{name}")),
        )
        .chain(
            GLOBAL_RISK_FEATURE_NAMES
                .iter()
                .map(|name| format!("g_missing_{name}")),
        )
        .collect()
}

pub fn direction_stockholm_close_global_risk_model_feature_names() -> Vec<String> {
    let global = stockholm_close_global_risk_feature_names();
    direction_model_feature_names()
        .into_iter()
        .chain(global.iter().map(|name| format!("g_{name}")))
        .chain(global.iter().map(|name| format!("g_missing_{name}")))
        .collect()
}

/// Build the finalized absolute-return direction matrix. No later observation
/// can change a row's features: all inputs end at `date`, while only the label
/// reads the primary index's following closing levels.
pub fn direction_training_matrix(
    series: &[MarketIndexSeries],
    start: Date,
    end: Date,
    horizon_sessions: usize,
) -> Result<DirectionTrainingMatrix, String> {
    direction_training_matrix_with_global_risk(series, start, end, horizon_sessions, &[])
}

/// Build direction features with optional completed-session global risk
/// context. When supplied, global observations are exposed only from UTC
/// dates strictly before the Stockholm decision date.
pub fn direction_training_matrix_with_global_risk(
    series: &[MarketIndexSeries],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    global_risk: &[GlobalRiskBar],
) -> Result<DirectionTrainingMatrix, String> {
    direction_training_matrix_with_global_risk_sources(
        series,
        start,
        end,
        horizon_sessions,
        global_risk,
        &[],
    )
}

/// Build direction features with same-date cross-asset futures context known
/// by the Stockholm cash close. The resulting action is executable only from
/// the following exchange session, as enforced by the unchanged label.
pub fn direction_training_matrix_with_stockholm_close_global_risk(
    series: &[MarketIndexSeries],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    stockholm_close_global_risk: &[GlobalRiskSeries],
) -> Result<DirectionTrainingMatrix, String> {
    direction_training_matrix_with_global_risk_sources(
        series,
        start,
        end,
        horizon_sessions,
        &[],
        stockholm_close_global_risk,
    )
}

fn direction_training_matrix_with_global_risk_sources(
    series: &[MarketIndexSeries],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    global_risk: &[GlobalRiskBar],
    stockholm_close_global_risk: &[GlobalRiskSeries],
) -> Result<DirectionTrainingMatrix, String> {
    if end < start {
        return Err("direction matrix end precedes start".into());
    }
    if horizon_sessions == 0 {
        return Err("direction label horizon must be positive".into());
    }
    let mut by_symbol = BTreeMap::new();
    let mut positions = BTreeMap::new();
    for history in series {
        validate_market_index_series(history)?;
        if by_symbol.insert(history.symbol.as_str(), history).is_some() {
            return Err(format!("duplicate direction index {}", history.symbol));
        }
        positions.insert(
            history.symbol.as_str(),
            history
                .bars
                .iter()
                .enumerate()
                .map(|(index, bar)| (bar.date, index))
                .collect::<BTreeMap<_, _>>(),
        );
    }
    for symbol in DIRECTION_INDEX_SYMBOLS {
        if !by_symbol.contains_key(symbol) {
            return Err(format!("direction index {symbol} is required"));
        }
    }
    let primary = by_symbol["OMXSGI"];
    let primary_positions = &positions["OMXSGI"];
    if !global_risk.is_empty() && !stockholm_close_global_risk.is_empty() {
        return Err("direction matrix cannot mix prior-day and Stockholm-close global risk".into());
    }
    let global_risk_values = global_risk_features(global_risk)?;
    let stockholm_close_global_risk_values = if stockholm_close_global_risk.is_empty() {
        BTreeMap::new()
    } else {
        stockholm_close_global_risk_features(stockholm_close_global_risk)?
    };
    let names = if !stockholm_close_global_risk.is_empty() {
        direction_stockholm_close_global_risk_model_feature_names()
    } else if global_risk.is_empty() {
        direction_model_feature_names()
    } else {
        direction_global_risk_model_feature_names()
    };
    let mut rows = Vec::new();
    for index in 252..primary.bars.len() {
        let decision = primary.bars[index].date;
        if decision < start || decision > end {
            continue;
        }
        let Some(exit_index) = index.checked_add(1 + horizon_sessions) else {
            continue;
        };
        if exit_index >= primary.bars.len() {
            continue;
        }
        let close = primary.bars[index].end_value;
        let primary_return = |window| index_return(primary, index, window);
        let ret_21 = primary_return(21);
        let ret_63 = primary_return(63);
        let ret_126 = primary_return(126);
        let ma50 = index_mean(primary, index, 50);
        let ma200 = index_mean(primary, index, 200);
        let mut raw = vec![
            primary_return(5),
            ret_21,
            ret_63,
            ret_126,
            primary_return(252),
            ma50.and_then(|value| finite(close / value - 1.0)),
            ma50.zip(ma200)
                .and_then(|(fast, slow)| finite(fast / slow - 1.0)),
            index_volatility(primary, index, 20),
            index_volatility(primary, index, 60),
            index_max_drawdown(primary, index, 126),
        ];
        for symbol in ["OMXS30GI", "OMXSBGI"] {
            let history = by_symbol[symbol];
            let current = positions[symbol].get(&decision).copied();
            for (window, market_return) in [(21, ret_21), (63, ret_63), (126, ret_126)] {
                raw.push(
                    current
                        .and_then(|secondary_index| index_return(history, secondary_index, window))
                        .zip(market_return)
                        .and_then(|(secondary_return, market_return)| {
                            finite(secondary_return - market_return)
                        }),
                );
            }
        }
        let sector_returns = |window| {
            DIRECTION_SECTOR_SYMBOLS
                .iter()
                .filter_map(|symbol| {
                    let history = by_symbol[symbol];
                    let current = positions[symbol].get(&decision).copied()?;
                    index_return(history, current, window)
                })
                .collect::<Vec<_>>()
        };
        let sector_21 = sector_returns(21);
        let sector_63 = sector_returns(63);
        let sector_126 = sector_returns(126);
        for values in [&sector_21, &sector_63, &sector_126] {
            raw.push(index_positive_fraction(values));
        }
        for values in [&sector_21, &sector_63, &sector_126] {
            let mut values = values.clone();
            raw.push((values.len() >= 4).then(|| median(&mut values)).flatten());
        }
        for values in [&sector_21, &sector_63] {
            raw.push(
                (values.len() >= 4)
                    .then(|| standard_deviation(values))
                    .flatten(),
            );
        }
        if raw.len() != DIRECTION_RAW_FEATURE_NAMES.len() {
            return Err("direction feature row has the wrong width".into());
        }
        let annualised_volatility_20 =
            raw[7].ok_or_else(|| format!("missing OMXSGI volatility on {decision}"))?;
        let mut finalized = Vec::with_capacity(names.len());
        finalized.extend(raw.iter().map(|value| value.unwrap_or(0.0)));
        finalized.extend(raw.iter().map(|value| f64::from(value.is_none())));
        if !global_risk.is_empty() {
            let global = global_risk_before(&global_risk_values, decision);
            finalized.extend(global.iter().map(|value| value.unwrap_or(0.0)));
            finalized.extend(global.iter().map(|value| f64::from(value.is_none())));
        } else if !stockholm_close_global_risk.is_empty() {
            let global = stockholm_close_global_risk_values
                .get(&decision)
                .cloned()
                .unwrap_or_else(|| vec![None; stockholm_close_global_risk_feature_names().len()]);
            finalized.extend(global.iter().map(|value| value.unwrap_or(0.0)));
            finalized.extend(global.iter().map(|value| f64::from(value.is_none())));
        }
        if finalized.iter().any(|value| !value.is_finite()) {
            return Err(format!("non-finite direction feature on {decision}"));
        }
        // The first tradable session's own close, not its `start_value`: the
        // archive's SOD is the prior session's close (dividend-adjusted), so a
        // label anchored there begins at the decision close and collects the
        // overnight gap into the first held session, which nobody deciding at
        // that close can trade. One full session of gap is deliberately
        // forfeited in exchange for a label the replay can act on.
        let entry_value = primary.bars[index + 1].end_value;
        let exit_value = primary.bars[exit_index].end_value;
        let target = exit_value / entry_value - 1.0;
        if !target.is_finite() {
            return Err(format!("non-finite OMXSGI direction label on {decision}"));
        }
        debug_assert_eq!(primary_positions[&decision], index);
        rows.push(DirectionTrainingRow {
            date: decision,
            target,
            sign_target: target.signum(),
            entry_value,
            exit_value,
            annualised_volatility_20,
            features: names.iter().cloned().zip(finalized).collect(),
        });
    }
    Ok(DirectionTrainingMatrix {
        features: names,
        rows,
    })
}

fn validate_market_index_series(series: &MarketIndexSeries) -> Result<(), String> {
    if series.symbol.trim().is_empty() {
        return Err("direction index symbol is empty".into());
    }
    for bar in &series.bars {
        if !bar.start_value.is_finite()
            || !bar.end_value.is_finite()
            || bar.start_value <= 0.0
            || bar.end_value <= 0.0
        {
            return Err(format!(
                "direction index {} has invalid values on {}",
                series.symbol, bar.date
            ));
        }
    }
    if series
        .bars
        .windows(2)
        .any(|pair| pair[0].date >= pair[1].date)
    {
        return Err(format!(
            "direction index {} must be strictly increasing and unique",
            series.symbol
        ));
    }
    Ok(())
}

fn index_return(series: &MarketIndexSeries, index: usize, window: usize) -> Option<f64> {
    let start = index.checked_sub(window)?;
    finite(series.bars[index].end_value / series.bars[start].end_value - 1.0)
}

fn index_mean(series: &MarketIndexSeries, index: usize, window: usize) -> Option<f64> {
    let start = index.checked_add(1)?.checked_sub(window)?;
    finite(
        series.bars[start..=index]
            .iter()
            .map(|bar| bar.end_value)
            .sum::<f64>()
            / window as f64,
    )
}

fn index_volatility(series: &MarketIndexSeries, index: usize, window: usize) -> Option<f64> {
    let start = index.checked_add(1)?.checked_sub(window)?;
    if start == 0 {
        return None;
    }
    let returns = (start..=index)
        .map(|current| (series.bars[current].end_value / series.bars[current - 1].end_value).ln())
        .collect::<Vec<_>>();
    finite(standard_deviation(&returns)? * 252.0_f64.sqrt())
}

fn index_max_drawdown(series: &MarketIndexSeries, index: usize, window: usize) -> Option<f64> {
    let start = index.checked_add(1)?.checked_sub(window)?;
    let mut peak = series.bars[start].end_value;
    let mut drawdown = 0.0_f64;
    for bar in &series.bars[start..=index] {
        peak = peak.max(bar.end_value);
        drawdown = drawdown.min(bar.end_value / peak - 1.0);
    }
    finite(drawdown)
}

fn index_positive_fraction(values: &[f64]) -> Option<f64> {
    (values.len() >= 4)
        .then(|| values.iter().filter(|value| **value > 0.0).count() as f64 / values.len() as f64)
        .and_then(finite)
}

/// Causal inputs to the shared stateful direction policy. The five component
/// spreads/returns remain visible so replay reports can explain a score rather
/// than exposing only a regime label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketTrendObservation {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub version: String,
    pub score: f64,
    pub price_vs_ma50: f64,
    pub ma50_vs_ma200: f64,
    pub return_63: f64,
    pub return_126: f64,
    pub return_252: f64,
    pub annualised_volatility_20: f64,
}

/// Produce one observation per sufficiently mature market session. Every
/// value at `t` uses EOD levels no later than `t`; the resulting decision can
/// therefore first trade on the next session.
pub fn market_trend(bars: &[MarketBar]) -> Result<Vec<MarketTrendObservation>, String> {
    if bars.is_empty() {
        return Ok(Vec::new());
    }
    for bar in bars {
        if !bar.close.is_finite() || bar.close <= 0.0 {
            return Err(format!("market bar on {} has an invalid close", bar.date));
        }
    }
    for pair in bars.windows(2) {
        if pair[0].date >= pair[1].date {
            return Err("market bars must be strictly increasing and unique".into());
        }
    }

    let mut observations = Vec::with_capacity(bars.len().saturating_sub(252));
    for index in 252..bars.len() {
        let close = bars[index].close;
        let ma50 = market_mean(bars, index, 50).expect("50 sessions exist");
        let ma200 = market_mean(bars, index, 200).expect("200 sessions exist");
        let return_63 = close / bars[index - 63].close - 1.0;
        let return_126 = close / bars[index - 126].close - 1.0;
        let return_252 = close / bars[index - 252].close - 1.0;
        let price_vs_ma50 = close / ma50 - 1.0;
        let ma50_vs_ma200 = ma50 / ma200 - 1.0;
        let components = [
            price_vs_ma50,
            ma50_vs_ma200,
            return_63,
            return_126,
            return_252,
        ];
        let score =
            components.iter().map(|value| vote(*value)).sum::<f64>() / components.len() as f64;
        let log_returns = (index - 19..=index)
            .map(|current| (bars[current].close / bars[current - 1].close).ln())
            .collect::<Vec<_>>();
        let annualised_volatility_20 = standard_deviation(&log_returns)
            .expect("twenty returns have variance")
            * 252.0_f64.sqrt();
        observations.push(MarketTrendObservation {
            date: bars[index].date,
            version: MARKET_TREND_VERSION.into(),
            score,
            price_vs_ma50,
            ma50_vs_ma200,
            return_63,
            return_126,
            return_252,
            annualised_volatility_20,
        });
    }
    Ok(observations)
}

fn market_mean(bars: &[MarketBar], index: usize, window: usize) -> Option<f64> {
    let start = index.checked_add(1)?.checked_sub(window)?;
    finite(bars[start..=index].iter().map(|bar| bar.close).sum::<f64>() / window as f64)
}

fn vote(value: f64) -> f64 {
    if value > 0.0 {
        1.0
    } else if value < 0.0 {
        -1.0
    } else {
        0.0
    }
}

impl DailyBar {
    pub fn validate(&self) -> Result<(), String> {
        if self.instrument_id.trim().is_empty() {
            return Err("empty instrument_id".into());
        }
        let prices = [
            self.raw_open,
            self.raw_high,
            self.raw_low,
            self.raw_close,
            self.adjusted_close,
        ];
        if prices
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return Err(format!(
                "{} on {} has a non-positive/non-finite price",
                self.instrument_id, self.date
            ));
        }
        if !self.volume.is_finite() || self.volume < 0.0 {
            return Err(format!(
                "{} on {} has invalid volume",
                self.instrument_id, self.date
            ));
        }
        if self.raw_high < self.raw_low
            || self.raw_open > self.raw_high
            || self.raw_open < self.raw_low
            || self.raw_close > self.raw_high
            || self.raw_close < self.raw_low
        {
            return Err(format!(
                "{} on {} has inconsistent raw OHLC",
                self.instrument_id, self.date
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureRow {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub instrument_id: String,
    /// Ordered exactly as [`FEATURE_NAMES`]. Nulls are retained until the
    /// point-in-time cross-section is normalised.
    pub values: Vec<Option<f64>>,
}

impl FeatureRow {
    pub fn value(&self, name: &str) -> Option<f64> {
        FEATURE_NAMES
            .iter()
            .position(|candidate| *candidate == name.trim_start_matches("x_"))
            .and_then(|index| self.values.get(index).copied().flatten())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalisedRow {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub instrument_id: String,
    /// All ranked `x_` values followed by their binary `m_` missing flags.
    /// No Python transformation remains.
    pub values: Vec<f64>,
}

pub fn model_feature_names() -> Vec<String> {
    value_feature_names()
        .into_iter()
        .map(|name| format!("x_{name}"))
        .chain(
            value_feature_names()
                .into_iter()
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn baseline_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .map(|name| format!("x_{name}"))
        .chain(FEATURE_NAMES.iter().map(|name| format!("m_{name}")))
        .collect()
}

pub fn baseline_global_risk_model_feature_names() -> Vec<String> {
    baseline_model_feature_names()
        .into_iter()
        .chain(
            GLOBAL_RISK_FEATURE_NAMES
                .iter()
                .map(|name| format!("g_{name}")),
        )
        .chain(
            GLOBAL_RISK_FEATURE_NAMES
                .iter()
                .map(|name| format!("g_missing_{name}")),
        )
        .collect()
}

pub fn stockholm_close_global_risk_feature_names() -> Vec<String> {
    STOCKHOLM_CLOSE_GLOBAL_RISK_SYMBOLS
        .iter()
        .flat_map(|symbol| {
            GLOBAL_RISK_FEATURE_NAMES.iter().map(move |name| {
                format!(
                    "{}_{}",
                    symbol.to_ascii_lowercase(),
                    name.strip_prefix("es_").unwrap_or(name)
                )
            })
        })
        .collect()
}

pub fn pdmr_microstructure_borrow_news_global_risk_model_feature_names() -> Vec<String> {
    let global = stockholm_close_global_risk_feature_names();
    pdmr_microstructure_borrow_news_model_feature_names()
        .into_iter()
        .chain(global.iter().map(|name| format!("g_{name}")))
        .chain(global.iter().map(|name| format!("g_missing_{name}")))
        .collect()
}

pub fn residual_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

/// Task 7(d): amended in place to drop the position-date-keyed FI public-short
/// family (late filings backfill `position_date`, a real look-ahead risk for
/// a candidate model). No version bump: Task 5 just renumbered every
/// candidate set and no real matrix has been built under the new numbers
/// yet, so this amendment breaks nothing that exists. The contract is now
/// identical to `residual_model_feature_names`; it survives as a distinct
/// `FeatureSet`/version string for continuity, not because its content
/// still differs. The retired family lives on only behind
/// `diagnostics_public_short_lookahead_model_feature_names`.
pub fn public_short_model_feature_names() -> Vec<String> {
    residual_model_feature_names()
}

/// Diagnostics-only counterpart of `public_short_model_feature_names` that
/// keeps the retired, position-date-keyed FI public-short family. Never use
/// this for a candidate model; it exists so the look-ahead-prone family and
/// its `PublicShortCursor` stay reachable for research.
pub fn diagnostics_public_short_lookahead_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .chain(PUBLIC_SHORT_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .chain(PUBLIC_SHORT_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn pdmr_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .chain(PDMR_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .chain(PDMR_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn pdmr_report_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .chain(PDMR_FEATURE_NAMES)
        .chain(REPORT_EVENT_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .chain(PDMR_FEATURE_NAMES)
                .chain(REPORT_EVENT_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn fundamental_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .chain(FUNDAMENTAL_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .chain(FUNDAMENTAL_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn quarterly_fundamental_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .chain(QUARTERLY_FUNDAMENTAL_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .chain(QUARTERLY_FUNDAMENTAL_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn pdmr_macro_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .chain(PDMR_FEATURE_NAMES)
        .chain(MACRO_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .chain(PDMR_FEATURE_NAMES)
                .chain(MACRO_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn pdmr_microstructure_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .chain(PDMR_FEATURE_NAMES)
        .chain(MICROSTRUCTURE_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .chain(PDMR_FEATURE_NAMES)
                .chain(MICROSTRUCTURE_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn pdmr_microstructure_borrow_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .chain(PDMR_FEATURE_NAMES)
        .chain(MICROSTRUCTURE_FEATURE_NAMES)
        .chain(BORROW_FEE_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .chain(PDMR_FEATURE_NAMES)
                .chain(MICROSTRUCTURE_FEATURE_NAMES)
                .chain(BORROW_FEE_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn pdmr_microstructure_borrow_news_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .chain(PDMR_FEATURE_NAMES)
        .chain(MICROSTRUCTURE_FEATURE_NAMES)
        .chain(BORROW_FEE_FEATURE_NAMES)
        .chain(COMPANY_NEWS_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .chain(PDMR_FEATURE_NAMES)
                .chain(MICROSTRUCTURE_FEATURE_NAMES)
                .chain(BORROW_FEE_FEATURE_NAMES)
                .chain(COMPANY_NEWS_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

pub fn pdmr_microstructure_borrow_news_report_text_model_feature_names() -> Vec<String> {
    FEATURE_NAMES
        .iter()
        .chain(RESIDUAL_FEATURE_NAMES)
        .chain(PDMR_FEATURE_NAMES)
        .chain(MICROSTRUCTURE_FEATURE_NAMES)
        .chain(BORROW_FEE_FEATURE_NAMES)
        .chain(COMPANY_NEWS_FEATURE_NAMES)
        .chain(REPORT_TEXT_FEATURE_NAMES)
        .map(|name| format!("x_{name}"))
        .chain(
            FEATURE_NAMES
                .iter()
                .chain(RESIDUAL_FEATURE_NAMES)
                .chain(PDMR_FEATURE_NAMES)
                .chain(MICROSTRUCTURE_FEATURE_NAMES)
                .chain(BORROW_FEE_FEATURE_NAMES)
                .chain(COMPANY_NEWS_FEATURE_NAMES)
                .chain(REPORT_TEXT_FEATURE_NAMES)
                .map(|name| format!("m_{name}")),
        )
        .collect()
}

fn value_feature_names() -> Vec<&'static str> {
    FEATURE_NAMES
        .iter()
        .chain(CONTEXT_FEATURE_NAMES)
        .copied()
        .collect()
}

pub fn validate_selection(names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Err("model selects no Stockholm features".into());
    }
    let all: BTreeSet<_> = model_feature_names()
        .into_iter()
        .chain(baseline_global_risk_model_feature_names())
        .chain(residual_model_feature_names())
        .chain(public_short_model_feature_names())
        .chain(pdmr_model_feature_names())
        .chain(pdmr_report_model_feature_names())
        .chain(fundamental_model_feature_names())
        .chain(quarterly_fundamental_model_feature_names())
        .chain(pdmr_macro_model_feature_names())
        .chain(pdmr_microstructure_model_feature_names())
        .chain(pdmr_microstructure_borrow_model_feature_names())
        .chain(pdmr_microstructure_borrow_news_model_feature_names())
        .chain(pdmr_microstructure_borrow_news_global_risk_model_feature_names())
        .chain(pdmr_microstructure_borrow_news_report_text_model_feature_names())
        .collect();
    let mut seen = BTreeSet::new();
    for name in names {
        if !all.contains(name) {
            return Err(format!("unknown Stockholm feature {name:?}"));
        }
        if !seen.insert(name) {
            return Err(format!("duplicate Stockholm feature {name:?}"));
        }
    }
    Ok(())
}

/// Calculate trailing rows independently per stable instrument identity.
/// Sorting is internal; duplicate dates are rejected rather than guessed.
pub fn daily(bars: &[DailyBar]) -> Result<Vec<FeatureRow>, String> {
    let mut grouped: BTreeMap<&str, Vec<&DailyBar>> = BTreeMap::new();
    for bar in bars {
        bar.validate()?;
        grouped.entry(&bar.instrument_id).or_default().push(bar);
    }
    let mut rows = Vec::with_capacity(bars.len());
    for (instrument_id, mut history) in grouped {
        history.sort_by_key(|bar| bar.date);
        for pair in history.windows(2) {
            if pair[0].date == pair[1].date {
                return Err(format!(
                    "duplicate {} bar on {}",
                    instrument_id, pair[0].date
                ));
            }
        }
        for index in 0..history.len() {
            let bar = history[index];
            let returns_60 = log_returns(&history, index, 60);
            let values = vec![
                simple_return(&history, index, 1),
                simple_return(&history, index, 5),
                simple_return(&history, index, 21),
                simple_return(&history, index, 63),
                simple_return(&history, index, 126),
                simple_return(&history, index, 252),
                skipped_return(&history, index, 21, 5),
                standard_deviation(&log_returns(&history, index, 20)),
                standard_deviation(&returns_60),
                downside_deviation(&returns_60),
                skew(&returns_60),
                max_drawdown(&history, index, 126),
                distance_from_high(&history, index, 252),
                distance_from_low(&history, index, 252),
                index.checked_sub(1).and_then(|previous| {
                    finite(adjusted_open(bar) / history[previous].adjusted_close - 1.0)
                }),
                finite((bar.raw_high - bar.raw_low) / bar.raw_close),
                close_location(bar),
                median_notional(&history, index, 20),
                amihud(&history, index, 20),
                volume_surge(&history, index, 20),
            ];
            debug_assert_eq!(values.len(), FEATURE_NAMES.len());
            rows.push(FeatureRow {
                date: bar.date,
                instrument_id: instrument_id.to_owned(),
                values,
            });
        }
    }
    rows.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then_with(|| a.instrument_id.cmp(&b.instrument_id))
    });
    Ok(rows)
}

/// Final point-in-time preprocessing shared by training and inference.
pub fn normalise_cross_section(rows: &[FeatureRow]) -> Result<Vec<NormalisedRow>, String> {
    let Some(date) = rows.first().map(|row| row.date) else {
        return Ok(Vec::new());
    };
    if rows.iter().any(|row| row.date != date) {
        return Err("Stockholm cross-section contains multiple dates".into());
    }
    if rows
        .iter()
        .any(|row| row.values.len() != FEATURE_NAMES.len())
    {
        return Err("Stockholm feature row has the wrong width".into());
    }
    let values = rows
        .iter()
        .map(|row| row.values.clone())
        .collect::<Vec<_>>();
    let normalised = features_common::rank_normalise(&values)?;
    Ok(rows
        .iter()
        .zip(normalised)
        .map(|(row, mut values)| {
            values.extend(row.values.iter().map(|value| f64::from(value.is_none())));
            NormalisedRow {
                date: row.date,
                instrument_id: row.instrument_id.clone(),
                values,
            }
        })
        .collect())
}

pub const LABEL_VERSION: &str = "forward-adjusted-open-5-v1";

pub fn label_version(horizon_sessions: usize) -> Result<String, String> {
    if horizon_sessions == 0 {
        return Err("label horizon must be positive".into());
    }
    Ok(format!("forward-adjusted-open-{horizon_sessions}-v1"))
}

pub fn label_horizon(version: &str) -> Option<usize> {
    version
        .strip_prefix("forward-adjusted-open-")?
        .strip_suffix("-v1")?
        .parse()
        .ok()
        .filter(|horizon| *horizon > 0)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UniverseBucket {
    LargeCap,
    MidCap,
    SmallCap,
    FirstNorthPremier,
    FirstNorth,
}

impl UniverseBucket {
    pub fn is_stockholm_main_market(&self) -> bool {
        matches!(self, Self::LargeCap | Self::MidCap | Self::SmallCap)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentMeta {
    pub instrument_id: String,
    pub symbol: String,
    pub isin: String,
    pub sector: String,
    pub bucket: UniverseBucket,
}

#[derive(Debug, Clone)]
pub struct CrossSectionInput {
    pub meta: InstrumentMeta,
    pub feature: FeatureRow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureSet {
    Baseline,
    BaselineGlobalRisk,
    Context,
    Residual,
    /// The candidate contract. Amended in place (see
    /// `DIAGNOSTICS_PUBLIC_SHORT_LOOKAHEAD_FEATURE_SET_VERSION`'s doc
    /// comment) to no longer carry the position-date-keyed public-short
    /// family; its feature list is now identical to `Residual`.
    ResidualPublicShort,
    /// Diagnostics-only: keeps the retired public-short family and its
    /// `PublicShortCursor` reachable for research. Never a candidate
    /// contract -- not wired into `validate_selection`, the runtime, or the
    /// CLI's `--feature-set` flag.
    DiagnosticsPublicShortLookahead,
    ResidualPdmr,
    ResidualPdmrReports,
    ResidualFundamentals,
    ResidualQuarterlyFundamentals,
    ResidualPdmrMacro,
    ResidualPdmrMicrostructure,
    ResidualPdmrMicrostructureBorrow,
    ResidualPdmrMicrostructureBorrowNews,
    ResidualPdmrMicrostructureBorrowNewsGlobalRisk,
    ResidualPdmrMicrostructureBorrowNewsReportText,
    ResidualPdmrMicrostructureBorrowNewsReportAttachments,
}

#[derive(Debug, Clone)]
struct ResidualFeatureRow {
    date: Date,
    instrument_id: String,
    values: Vec<Option<f64>>,
}

type ExternalFeatureRows = BTreeMap<(String, Date), Vec<Option<f64>>>;

fn residual_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
) -> Result<Vec<NormalisedRow>, String> {
    let Some(date) = inputs.first().map(|input| input.feature.date) else {
        return Ok(Vec::new());
    };
    let mut by_bucket: BTreeMap<UniverseBucket, Vec<usize>> = BTreeMap::new();
    for (index, input) in inputs.iter().enumerate() {
        by_bucket
            .entry(input.meta.bucket.clone())
            .or_default()
            .push(index);
    }
    let mut output = vec![Vec::new(); inputs.len()];
    for indexes in by_bucket.values() {
        let raw = indexes
            .iter()
            .map(|index| {
                let input = &inputs[*index];
                let extra = residual
                    .get(&(input.meta.instrument_id.clone(), date))
                    .ok_or_else(|| {
                        format!(
                            "missing residual features for {} on {date}",
                            input.meta.instrument_id
                        )
                    })?;
                if extra.date != date
                    || extra.instrument_id != input.meta.instrument_id
                    || extra.values.len() != RESIDUAL_FEATURE_NAMES.len()
                {
                    return Err("invalid Stockholm residual feature row".into());
                }
                Ok(input
                    .feature
                    .values
                    .iter()
                    .chain(&extra.values)
                    .copied()
                    .collect::<Vec<_>>())
            })
            .collect::<Result<Vec<_>, String>>()?;
        for ((slot, index), values) in indexes
            .iter()
            .enumerate()
            .zip(features_common::rank_normalise(&raw)?)
        {
            let mut finalized = values;
            finalized.extend(raw[slot].iter().map(|value| f64::from(value.is_none())));
            output[*index] = finalized;
        }
    }
    Ok(inputs
        .iter()
        .zip(output)
        .map(|(input, values)| NormalisedRow {
            date,
            instrument_id: input.meta.instrument_id.clone(),
            values,
        })
        .collect())
}

fn residual_external_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
    external: &BTreeMap<String, Vec<Option<f64>>>,
    external_width: usize,
    external_name: &str,
) -> Result<Vec<NormalisedRow>, String> {
    let Some(date) = inputs.first().map(|input| input.feature.date) else {
        return Ok(Vec::new());
    };
    let mut by_bucket: BTreeMap<UniverseBucket, Vec<usize>> = BTreeMap::new();
    for (index, input) in inputs.iter().enumerate() {
        by_bucket
            .entry(input.meta.bucket.clone())
            .or_default()
            .push(index);
    }
    let mut output = vec![Vec::new(); inputs.len()];
    for indexes in by_bucket.values() {
        let raw = indexes
            .iter()
            .map(|index| {
                let input = &inputs[*index];
                let residual = residual
                    .get(&(input.meta.instrument_id.clone(), date))
                    .ok_or_else(|| {
                        format!(
                            "missing residual features for {} on {date}",
                            input.meta.instrument_id
                        )
                    })?;
                let external = external.get(&input.meta.instrument_id).ok_or_else(|| {
                    format!(
                        "missing {external_name} features for {} on {date}",
                        input.meta.instrument_id,
                    )
                })?;
                if residual.values.len() != RESIDUAL_FEATURE_NAMES.len()
                    || external.len() != external_width
                {
                    return Err(format!("invalid Stockholm {external_name} feature row"));
                }
                Ok(input
                    .feature
                    .values
                    .iter()
                    .chain(&residual.values)
                    .chain(external)
                    .copied()
                    .collect::<Vec<_>>())
            })
            .collect::<Result<Vec<_>, String>>()?;
        for ((slot, index), values) in indexes
            .iter()
            .enumerate()
            .zip(features_common::rank_normalise(&raw)?)
        {
            let mut finalized = values;
            finalized.extend(raw[slot].iter().map(|value| f64::from(value.is_none())));
            output[*index] = finalized;
        }
    }
    Ok(inputs
        .iter()
        .zip(output)
        .map(|(input, values)| NormalisedRow {
            date,
            instrument_id: input.meta.instrument_id.clone(),
            values,
        })
        .collect())
}

fn residual_public_short_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
    public_short: &BTreeMap<String, Vec<Option<f64>>>,
) -> Result<Vec<NormalisedRow>, String> {
    residual_external_cross_section(
        inputs,
        residual,
        public_short,
        PUBLIC_SHORT_FEATURE_NAMES.len(),
        "public-short",
    )
}

fn residual_pdmr_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
    pdmr: &BTreeMap<String, Vec<Option<f64>>>,
) -> Result<Vec<NormalisedRow>, String> {
    residual_external_cross_section(inputs, residual, pdmr, PDMR_FEATURE_NAMES.len(), "PDMR")
}

fn residual_pdmr_report_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
    external: &BTreeMap<String, Vec<Option<f64>>>,
) -> Result<Vec<NormalisedRow>, String> {
    residual_external_cross_section(
        inputs,
        residual,
        external,
        PDMR_FEATURE_NAMES.len() + REPORT_EVENT_FEATURE_NAMES.len(),
        "PDMR/report-event",
    )
}

fn residual_fundamental_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
    fundamentals: &BTreeMap<String, Vec<Option<f64>>>,
) -> Result<Vec<NormalisedRow>, String> {
    residual_external_cross_section(
        inputs,
        residual,
        fundamentals,
        FUNDAMENTAL_FEATURE_NAMES.len(),
        "annual-fundamental",
    )
}

fn residual_pdmr_macro_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
    external: &BTreeMap<String, Vec<Option<f64>>>,
) -> Result<Vec<NormalisedRow>, String> {
    residual_external_cross_section(
        inputs,
        residual,
        external,
        PDMR_FEATURE_NAMES.len() + MACRO_FEATURE_NAMES.len(),
        "PDMR/macro",
    )
}

fn residual_pdmr_microstructure_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
    external: &BTreeMap<String, Vec<Option<f64>>>,
) -> Result<Vec<NormalisedRow>, String> {
    residual_external_cross_section(
        inputs,
        residual,
        external,
        PDMR_FEATURE_NAMES.len() + MICROSTRUCTURE_FEATURE_NAMES.len(),
        "PDMR/microstructure",
    )
}

fn residual_pdmr_microstructure_borrow_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
    external: &BTreeMap<String, Vec<Option<f64>>>,
) -> Result<Vec<NormalisedRow>, String> {
    residual_external_cross_section(
        inputs,
        residual,
        external,
        PDMR_FEATURE_NAMES.len()
            + MICROSTRUCTURE_FEATURE_NAMES.len()
            + BORROW_FEE_FEATURE_NAMES.len(),
        "PDMR/microstructure/borrow",
    )
}

fn residual_pdmr_microstructure_borrow_news_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
    external: &BTreeMap<String, Vec<Option<f64>>>,
) -> Result<Vec<NormalisedRow>, String> {
    residual_external_cross_section(
        inputs,
        residual,
        external,
        PDMR_FEATURE_NAMES.len()
            + MICROSTRUCTURE_FEATURE_NAMES.len()
            + BORROW_FEE_FEATURE_NAMES.len()
            + COMPANY_NEWS_FEATURE_NAMES.len(),
        "PDMR/microstructure/borrow/company-news",
    )
}

fn residual_pdmr_microstructure_borrow_news_report_text_cross_section(
    inputs: &[CrossSectionInput],
    residual: &BTreeMap<(String, Date), ResidualFeatureRow>,
    external: &BTreeMap<String, Vec<Option<f64>>>,
) -> Result<Vec<NormalisedRow>, String> {
    residual_external_cross_section(
        inputs,
        residual,
        external,
        PDMR_FEATURE_NAMES.len()
            + MICROSTRUCTURE_FEATURE_NAMES.len()
            + BORROW_FEE_FEATURE_NAMES.len()
            + COMPANY_NEWS_FEATURE_NAMES.len()
            + REPORT_TEXT_FEATURE_NAMES.len(),
        "PDMR/microstructure/borrow/company-news/report-text",
    )
}

/// Provider-neutral public short-position event. `None` represents a
/// threshold-exit disclosure (`<0.5%`), not a measured zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicShortPositionEvent {
    pub holder: String,
    pub isin: String,
    #[serde(with = "date_serde")]
    pub position_date: Date,
    pub position_percent: Option<f64>,
}

#[derive(Debug, Default)]
struct PublicShortState {
    holders: BTreeMap<String, f64>,
    changes: Vec<(Date, f64)>,
    event_dates: Vec<Date>,
}

struct PublicShortCursor<'a> {
    events: Vec<&'a PublicShortPositionEvent>,
    next: usize,
    first_event_date: Date,
    states: BTreeMap<String, PublicShortState>,
}

impl<'a> PublicShortCursor<'a> {
    fn new(events: &'a [PublicShortPositionEvent]) -> Result<Self, String> {
        if events.is_empty() {
            return Err("public-short feature set requires position events".into());
        }
        for event in events {
            if event.holder.trim().is_empty() || event.isin.trim().is_empty() {
                return Err("public-short event has an empty holder or ISIN".into());
            }
            if event
                .position_percent
                .is_some_and(|value| !value.is_finite() || value < 0.0)
            {
                return Err("public-short event has an invalid position".into());
            }
        }
        let mut sorted = events.iter().collect::<Vec<_>>();
        sorted.sort_by(|a, b| {
            a.position_date
                .cmp(&b.position_date)
                .then_with(|| a.isin.cmp(&b.isin))
                .then_with(|| a.holder.cmp(&b.holder))
        });
        Ok(Self {
            first_event_date: sorted[0].position_date,
            events: sorted,
            next: 0,
            states: BTreeMap::new(),
        })
    }

    fn advance_before(&mut self, decision_date: Date) {
        while self.next < self.events.len() && self.events[self.next].position_date < decision_date
        {
            let event = self.events[self.next];
            let state = self.states.entry(event.isin.clone()).or_default();
            let old = state.holders.get(&event.holder).copied().unwrap_or(0.0);
            let new = event.position_percent.unwrap_or(0.0);
            if event.position_percent.is_some() {
                state.holders.insert(event.holder.clone(), new);
            } else {
                state.holders.remove(&event.holder);
            }
            state.changes.push((event.position_date, new - old));
            state.event_dates.push(event.position_date);
            self.next += 1;
        }
    }

    fn values(&self, isin: &str, decision_date: Date) -> Vec<Option<f64>> {
        if decision_date <= self.first_event_date {
            return vec![None; PUBLIC_SHORT_FEATURE_NAMES.len()];
        }
        let Some(state) = self.states.get(isin) else {
            // Once the register exists, this means no holder has crossed the
            // public threshold, which is observable zero disclosed demand.
            return vec![Some(0.0), Some(0.0), Some(0.0), Some(0.0), None, Some(0.0)];
        };
        let since_30 = decision_date - time::Duration::days(30);
        let since_90 = decision_date - time::Duration::days(90);
        let change_30 = state
            .changes
            .iter()
            .filter(|(date, _)| *date >= since_30)
            .map(|(_, change)| change)
            .sum::<f64>();
        let change_90 = state
            .changes
            .iter()
            .filter(|(date, _)| *date >= since_90)
            .map(|(_, change)| change)
            .sum::<f64>();
        let last_event = state.event_dates.last().copied();
        let events_30 = state
            .event_dates
            .iter()
            .filter(|date| **date >= since_30)
            .count() as f64;
        vec![
            Some(state.holders.values().sum()),
            Some(state.holders.len() as f64),
            finite(change_30),
            finite(change_90),
            last_event.map(|date| (decision_date - date).whole_days() as f64),
            Some(events_30),
        ]
    }
}

/// Provider-neutral FI PDMR transaction. Availability is always controlled by
/// `publication_date`; `transaction_date` is descriptive and must never move
/// a filing into an earlier feature row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdmrTransactionEvent {
    #[serde(with = "date_serde")]
    pub publication_date: Date,
    #[serde(with = "date_serde")]
    pub transaction_date: Date,
    pub pdmr: String,
    pub isin: String,
    pub initial_notification: bool,
    pub linked_to_share_option_programme: bool,
    pub nature: String,
    pub instrument_type: String,
    pub volume: Option<f64>,
    pub unit: String,
    pub price: Option<f64>,
    pub currency: String,
}

impl PdmrTransactionEvent {
    fn direction(&self) -> Option<f64> {
        if !self.initial_notification
            || self.linked_to_share_option_programme
            || !self.instrument_type.trim().eq_ignore_ascii_case("share")
            || !self.unit.trim().eq_ignore_ascii_case("quantity")
        {
            return None;
        }
        let sign = if self.nature.trim().eq_ignore_ascii_case("acquisition") {
            1.0
        } else if self.nature.trim().eq_ignore_ascii_case("disposal") {
            -1.0
        } else {
            return None;
        };
        self.volume
            .filter(|value| value.is_finite() && *value > 0.0)?;
        self.price
            .filter(|value| value.is_finite() && *value > 0.0)?;
        Some(sign)
    }

    fn signed_sek_value(&self) -> Option<f64> {
        if !self.currency.trim().eq_ignore_ascii_case("sek") {
            return None;
        }
        let sign = self.direction()?;
        let volume = self.volume?;
        let price = self.price?;
        finite(sign * volume * price)
    }
}

struct PdmrCursor<'a> {
    events: Vec<&'a PdmrTransactionEvent>,
    next: usize,
    first_publication_date: Date,
    by_isin: BTreeMap<String, Vec<&'a PdmrTransactionEvent>>,
}

impl<'a> PdmrCursor<'a> {
    fn new(events: &'a [PdmrTransactionEvent]) -> Result<Self, String> {
        if events.is_empty() {
            return Err("PDMR feature set requires transaction events".into());
        }
        for event in events {
            if event.isin.trim().is_empty() {
                return Err("PDMR event has an empty ISIN".into());
            }
            if event
                .volume
                .is_some_and(|value| !value.is_finite() || value < 0.0)
                || event
                    .price
                    .is_some_and(|value| !value.is_finite() || value < 0.0)
            {
                return Err("PDMR event has an invalid volume or price".into());
            }
        }
        let mut sorted = events.iter().collect::<Vec<_>>();
        sorted.sort_by(|a, b| {
            a.publication_date
                .cmp(&b.publication_date)
                .then_with(|| a.isin.cmp(&b.isin))
                .then_with(|| a.pdmr.cmp(&b.pdmr))
        });
        Ok(Self {
            first_publication_date: sorted[0].publication_date,
            events: sorted,
            next: 0,
            by_isin: BTreeMap::new(),
        })
    }

    fn advance_before(&mut self, decision_date: Date) {
        while self.next < self.events.len()
            && self.events[self.next].publication_date < decision_date
        {
            let event = self.events[self.next];
            if event.direction().is_some() {
                self.by_isin
                    .entry(event.isin.clone())
                    .or_default()
                    .push(event);
            }
            self.next += 1;
        }
    }

    fn values(&self, isin: &str, decision_date: Date) -> Vec<Option<f64>> {
        if decision_date <= self.first_publication_date {
            return vec![None; PDMR_FEATURE_NAMES.len()];
        }
        let Some(events) = self.by_isin.get(isin) else {
            return vec![
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                None,
            ];
        };
        let since_30 = decision_date - time::Duration::days(30);
        let since_90 = decision_date - time::Duration::days(90);
        let mut net_30 = 0.0;
        let mut net_90 = 0.0;
        let mut buys_90 = 0.0;
        let mut sells_90 = 0.0;
        let mut transactions_30 = 0_usize;
        let mut buyers_90 = BTreeSet::new();
        let mut last_buy = None;
        for event in events.iter().rev() {
            if event.publication_date < since_90 {
                break;
            }
            let direction = event
                .direction()
                .expect("PDMR cursor contains only qualifying transactions");
            if let Some(value) = event.signed_sek_value() {
                net_90 += value;
                if value > 0.0 {
                    buys_90 += value;
                } else {
                    sells_90 -= value;
                }
                if event.publication_date >= since_30 {
                    net_30 += value;
                }
            }
            if direction > 0.0 {
                if !event.pdmr.trim().is_empty() {
                    buyers_90.insert(event.pdmr.trim().to_lowercase());
                }
                last_buy.get_or_insert(event.publication_date);
            }
            if event.publication_date >= since_30 {
                transactions_30 += 1;
            }
        }
        if last_buy.is_none() {
            last_buy = events.iter().rev().find_map(|event| {
                (event.direction().is_some_and(|direction| direction > 0.0))
                    .then_some(event.publication_date)
            });
        }
        vec![
            finite(net_30),
            finite(net_90),
            finite(buys_90),
            finite(sells_90),
            Some(transactions_30 as f64),
            Some(buyers_90.len() as f64),
            last_buy.map(|date| (decision_date - date).whole_days() as f64),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanyNewsKind {
    InsideInformation,
    OwnShares,
    Management,
    Prospectus,
    MajorShareholder,
    TenderOffer,
    Other,
}

/// Provider-neutral Nasdaq issuer disclosure after venue/category decoding
/// and issuer-name mapping have been resolved by the data adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyNewsEvent {
    pub instrument_id: String,
    #[serde(with = "date_serde")]
    pub publication_date: Date,
    pub publication_key: String,
    pub after_market_close: bool,
    pub kind: CompanyNewsKind,
}

/// Provider-neutral official financial-report body. HTTP/HTML decoding is a
/// shared equity-data responsibility; this crate alone owns the fixed numeric
/// interpretation used by training and inference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinancialReportTextEvent {
    pub instrument_id: String,
    #[serde(with = "date_serde")]
    pub publication_date: Date,
    /// Provider publication timestamp, used to deduplicate translations of
    /// one economic release without consulting future returns.
    pub publication_key: String,
    pub language: String,
    pub body_text: String,
    /// Optional finalized values in `REPORT_TEXT_FEATURE_NAMES[1..]` order.
    /// These may only be produced by [`report_text_metrics_with_supplements`]
    /// in this crate; the shared provider merely supplies document text.
    #[serde(default)]
    pub extracted_metrics: Option<Vec<Option<f64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportTextMetricCoverage {
    pub deduplicated_events: usize,
    pub events_with_any_metric: usize,
    pub by_feature: BTreeMap<String, usize>,
}

pub fn report_text_metric_coverage(
    events: &[FinancialReportTextEvent],
) -> Result<ReportTextMetricCoverage, String> {
    let cursor = FinancialReportTextCursor::new(events)?;
    let mut by_feature = REPORT_TEXT_FEATURE_NAMES[1..]
        .iter()
        .map(|name| ((*name).to_owned(), 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut events_with_any_metric = 0;
    for event in &cursor.events {
        if event.metrics.iter().any(Option::is_some) {
            events_with_any_metric += 1;
        }
        for (name, value) in REPORT_TEXT_FEATURE_NAMES[1..].iter().zip(&event.metrics) {
            if value.is_some() {
                *by_feature.get_mut(*name).expect("coverage name exists") += 1;
            }
        }
    }
    Ok(ReportTextMetricCoverage {
        deduplicated_events: cursor.events.len(),
        events_with_any_metric,
        by_feature,
    })
}

/// Apply the fixed Rust accounting parser to an announcement body and any
/// official same-release document texts. Body values win; documents fill only
/// fields the body did not declare, so adding a presentation cannot silently
/// replace a value already published in the message.
pub fn report_text_metrics_with_supplements<'a>(
    body_text: &str,
    supplements: impl IntoIterator<Item = &'a str>,
) -> Vec<Option<f64>> {
    let mut metrics = extract_report_text_metrics(body_text);
    for text in supplements {
        let candidate = extract_report_text_metrics(text);
        for (value, supplement) in metrics.iter_mut().zip(candidate) {
            if value.is_none() {
                *value = supplement;
            }
        }
    }
    metrics
}

#[derive(Debug, Clone)]
struct ExtractedReportText {
    instrument_id: String,
    publication_date: Date,
    publication_key: String,
    language: String,
    metrics: Vec<Option<f64>>,
}

struct FinancialReportTextCursor {
    events: Vec<ExtractedReportText>,
    next: usize,
    first_publication_date: Date,
    latest_by_instrument: BTreeMap<String, ExtractedReportText>,
}

impl FinancialReportTextCursor {
    fn new(events: &[FinancialReportTextEvent]) -> Result<Self, String> {
        if events.is_empty() {
            return Err("report-text feature set requires official report bodies".into());
        }
        if events.iter().any(|event| {
            event.instrument_id.trim().is_empty()
                || event.publication_key.trim().is_empty()
                || event.body_text.trim().is_empty()
        }) {
            return Err("report-text event has an empty instrument, key, or body".into());
        }
        let mut deduplicated = BTreeMap::<(String, String), ExtractedReportText>::new();
        for event in events {
            let metrics = if let Some(metrics) = &event.extracted_metrics {
                if metrics.len() != REPORT_TEXT_FEATURE_NAMES.len() - 1 {
                    return Err("report-text event has the wrong extracted metric width".into());
                }
                metrics.clone()
            } else {
                extract_report_text_metrics(&event.body_text)
            };
            let candidate = ExtractedReportText {
                instrument_id: event.instrument_id.clone(),
                publication_date: event.publication_date,
                publication_key: event.publication_key.clone(),
                language: event.language.clone(),
                metrics,
            };
            let key = (
                candidate.instrument_id.clone(),
                candidate.publication_key.clone(),
            );
            let candidate_coverage = candidate.metrics.iter().flatten().count();
            let replace = deduplicated.get(&key).is_none_or(|current| {
                let current_coverage = current.metrics.iter().flatten().count();
                candidate_coverage > current_coverage
                    || (candidate_coverage == current_coverage
                        && candidate.language == "en"
                        && current.language != "en")
            });
            if replace {
                deduplicated.insert(key, candidate);
            }
        }
        let mut events = deduplicated.into_values().collect::<Vec<_>>();
        events.sort_by(|a, b| {
            a.publication_date
                .cmp(&b.publication_date)
                .then_with(|| a.instrument_id.cmp(&b.instrument_id))
                .then_with(|| a.publication_key.cmp(&b.publication_key))
        });
        Ok(Self {
            first_publication_date: events[0].publication_date,
            events,
            next: 0,
            latest_by_instrument: BTreeMap::new(),
        })
    }

    fn advance_before(&mut self, decision_date: Date) {
        while self.next < self.events.len()
            && self.events[self.next].publication_date < decision_date
        {
            let event = self.events[self.next].clone();
            self.latest_by_instrument
                .insert(event.instrument_id.clone(), event);
            self.next += 1;
        }
    }

    fn values(&self, instrument_id: &str, decision_date: Date) -> Vec<Option<f64>> {
        if decision_date <= self.first_publication_date {
            return vec![None; REPORT_TEXT_FEATURE_NAMES.len()];
        }
        let Some(event) = self.latest_by_instrument.get(instrument_id) else {
            return vec![None; REPORT_TEXT_FEATURE_NAMES.len()];
        };
        let mut values = vec![Some(
            (decision_date - event.publication_date).whole_days() as f64
        )];
        values.extend(event.metrics.iter().copied());
        debug_assert_eq!(values.len(), REPORT_TEXT_FEATURE_NAMES.len());
        values
    }
}

fn extract_report_text_metrics(text: &str) -> Vec<Option<f64>> {
    let sales = report_metric_pair(text, &REPORT_SALES_PAIR).and_then(report_growth);
    let order_intake = report_metric_pair(text, &REPORT_ORDER_INTAKE_PAIR).and_then(report_growth);
    let ebit = report_metric_pair(text, &REPORT_EBIT_PAIR).and_then(report_growth);
    let margin_pair = report_metric_pair(text, &REPORT_MARGIN_PAIR);
    let margin = margin_pair.and_then(|(current, _)| {
        (current.is_finite() && current.abs() <= 100.0).then_some(current / 100.0)
    });
    let margin_change = margin_pair.and_then(|(current, prior)| {
        let change = (current - prior) / 100.0;
        (change.is_finite() && change.abs() <= 1.0).then_some(change)
    });
    let eps = report_metric_pair(text, &REPORT_EPS_PAIR).and_then(report_growth);
    let dividend = report_metric_pair(text, &REPORT_DIVIDEND_PAIR).and_then(report_growth);
    vec![
        sales,
        order_intake,
        ebit,
        margin,
        margin_change,
        eps,
        dividend,
    ]
}

static REPORT_SALES_PAIR: LazyLock<Regex> =
    LazyLock::new(|| report_pair_regex(&["net sales", "revenue", "nettoomsättning"]));
static REPORT_ORDER_INTAKE_PAIR: LazyLock<Regex> =
    LazyLock::new(|| report_pair_regex(&["order intake", "orderingång"]));
static REPORT_EBIT_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    report_pair_regex(&[
        "adjusted ebit",
        "ebit",
        "operating profit",
        "rörelseresultat",
    ])
});
static REPORT_MARGIN_PAIR: LazyLock<Regex> =
    LazyLock::new(|| report_pair_regex(&["ebit margin", "operating margin", "rörelsemarginal"]));
static REPORT_EPS_PAIR: LazyLock<Regex> =
    LazyLock::new(|| report_pair_regex(&["earnings per share", "resultat per aktie"]));
static REPORT_DIVIDEND_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    report_pair_regex(&[
        "dividend per share",
        "utdelning per aktie",
        "dividend",
        "utdelning",
    ])
});

fn report_pair_regex(labels: &[&str]) -> Regex {
    let labels = labels
        .iter()
        .map(|label| regex::escape(label))
        .collect::<Vec<_>>()
        .join("|");
    let pattern = format!(
        r"(?is)(?:{labels})[^0-9+\-−]{{0,80}}([+\-−]?[0-9][0-9 .,]*?)\s*\(\s*([+\-−]?[0-9][0-9 .,]*?)\s*\)"
    );
    Regex::new(&pattern).expect("fixed report-pair regex is valid")
}

fn report_metric_pair(text: &str, regex: &Regex) -> Option<(f64, f64)> {
    let captures = regex.captures(text)?;
    Some((
        parse_report_number(captures.get(1)?.as_str())?,
        parse_report_number(captures.get(2)?.as_str())?,
    ))
}

fn parse_report_number(value: &str) -> Option<f64> {
    let mut value = value
        .replace('−', "-")
        .replace(['\u{a0}', ' '], "")
        .trim()
        .to_owned();
    if value.contains(',') && value.contains('.') {
        if value.rfind(',')? > value.rfind('.')? {
            value = value.replace('.', "").replace(',', ".");
        } else {
            value = value.replace(',', "");
        }
    } else {
        value = value.replace(',', ".");
    }
    value.parse::<f64>().ok().filter(|value| value.is_finite())
}

fn report_growth((current, prior): (f64, f64)) -> Option<f64> {
    if !current.is_finite() || !prior.is_finite() || prior.abs() < 1e-12 {
        return None;
    }
    let change = (current - prior) / prior.abs();
    (change.is_finite() && (-10.0..=10.0).contains(&change)).then_some(change)
}

struct CompanyNewsCursor<'a> {
    events: Vec<&'a CompanyNewsEvent>,
    next: usize,
    first_publication_date: Date,
    by_instrument: BTreeMap<String, Vec<&'a CompanyNewsEvent>>,
    histories: BTreeMap<String, Vec<&'a DailyBar>>,
}

impl<'a> CompanyNewsCursor<'a> {
    fn new(events: &'a [CompanyNewsEvent], bars: &'a [DailyBar]) -> Result<Self, String> {
        if events.is_empty() {
            return Err("company-news feature set requires disclosure events".into());
        }
        if events.iter().any(|event| {
            event.instrument_id.trim().is_empty() || event.publication_key.trim().is_empty()
        }) {
            return Err("company-news event has an empty instrument or publication key".into());
        }
        let mut sorted = events.iter().collect::<Vec<_>>();
        sorted.sort_by(|a, b| {
            a.publication_date
                .cmp(&b.publication_date)
                .then_with(|| a.instrument_id.cmp(&b.instrument_id))
                .then_with(|| a.publication_key.cmp(&b.publication_key))
        });
        sorted.dedup_by(|right, left| {
            left.instrument_id == right.instrument_id
                && left.publication_key == right.publication_key
        });
        let mut histories: BTreeMap<String, Vec<&DailyBar>> = BTreeMap::new();
        for bar in bars {
            histories
                .entry(bar.instrument_id.clone())
                .or_default()
                .push(bar);
        }
        for history in histories.values_mut() {
            history.sort_by_key(|bar| bar.date);
        }
        Ok(Self {
            first_publication_date: sorted[0].publication_date,
            events: sorted,
            next: 0,
            by_instrument: BTreeMap::new(),
            histories,
        })
    }

    fn advance_before(&mut self, decision_date: Date) {
        while self.next < self.events.len()
            && self.events[self.next].publication_date < decision_date
        {
            let event = self.events[self.next];
            self.by_instrument
                .entry(event.instrument_id.clone())
                .or_default()
                .push(event);
            self.next += 1;
        }
    }

    fn values(&self, instrument_id: &str, decision_date: Date) -> Vec<Option<f64>> {
        if decision_date <= self.first_publication_date {
            return vec![None; COMPANY_NEWS_FEATURE_NAMES.len()];
        }
        let Some(events) = self.by_instrument.get(instrument_id) else {
            return vec![
                Some(0.0),
                Some(0.0),
                None,
                Some(0.0),
                Some(0.0),
                None,
                None,
                None,
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
            ];
        };
        let since_30 = decision_date - time::Duration::days(30);
        let since_90 = decision_date - time::Duration::days(90);
        let count = |kind: CompanyNewsKind| {
            events
                .iter()
                .filter(|event| event.kind == kind && event.publication_date >= since_90)
                .count() as f64
        };
        let latest_inside = events
            .iter()
            .rev()
            .find(|event| event.kind == CompanyNewsKind::InsideInformation);
        let history = self.histories.get(instrument_id);
        vec![
            Some(
                events
                    .iter()
                    .filter(|event| event.publication_date >= since_30)
                    .count() as f64,
            ),
            Some(
                events
                    .iter()
                    .filter(|event| event.publication_date >= since_90)
                    .count() as f64,
            ),
            latest_inside.map(|event| (decision_date - event.publication_date).whole_days() as f64),
            Some(
                events
                    .iter()
                    .filter(|event| {
                        event.kind == CompanyNewsKind::InsideInformation
                            && event.publication_date >= since_30
                    })
                    .count() as f64,
            ),
            Some(count(CompanyNewsKind::InsideInformation)),
            history.and_then(|history| {
                latest_inside
                    .and_then(|event| company_news_reaction(history, event, decision_date, 1))
            }),
            history.and_then(|history| {
                latest_inside
                    .and_then(|event| company_news_reaction(history, event, decision_date, 5))
            }),
            history.and_then(|history| {
                latest_inside
                    .and_then(|event| company_news_reaction(history, event, decision_date, 21))
            }),
            Some(count(CompanyNewsKind::OwnShares)),
            Some(count(CompanyNewsKind::Management)),
            Some(count(CompanyNewsKind::Prospectus)),
            Some(count(CompanyNewsKind::MajorShareholder)),
            Some(count(CompanyNewsKind::TenderOffer)),
        ]
    }
}

fn company_news_reaction(
    history: &[&DailyBar],
    event: &CompanyNewsEvent,
    decision_date: Date,
    horizon_sessions: usize,
) -> Option<f64> {
    let baseline_end = if event.after_market_close {
        history.partition_point(|bar| bar.date <= event.publication_date)
    } else {
        history.partition_point(|bar| bar.date < event.publication_date)
    };
    let baseline_index = baseline_end.checked_sub(1)?;
    let current_end = history.partition_point(|bar| bar.date <= decision_date);
    let current_index = current_end.checked_sub(1)?;
    if current_index <= baseline_index {
        return None;
    }
    let reaction_index = baseline_index
        .checked_add(horizon_sessions)?
        .min(current_index);
    finite(history[reaction_index].adjusted_close / history[baseline_index].adjusted_close - 1.0)
}

/// Provider-neutral official financial-report disclosure after issuer-name
/// mapping has been resolved by the data adapter. `publication_key` preserves
/// the provider timestamp so translations can be deduplicated as one event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinancialReportEvent {
    pub instrument_id: String,
    #[serde(with = "date_serde")]
    pub publication_date: Date,
    pub publication_key: String,
    pub after_market_close: bool,
}

struct FinancialReportCursor<'a> {
    events: Vec<&'a FinancialReportEvent>,
    next: usize,
    first_publication_date: Date,
    by_instrument: BTreeMap<String, Vec<&'a FinancialReportEvent>>,
    histories: BTreeMap<String, Vec<&'a DailyBar>>,
}

impl<'a> FinancialReportCursor<'a> {
    fn new(events: &'a [FinancialReportEvent], bars: &'a [DailyBar]) -> Result<Self, String> {
        if events.is_empty() {
            return Err("report-event feature set requires disclosure events".into());
        }
        if events.iter().any(|event| {
            event.instrument_id.trim().is_empty() || event.publication_key.trim().is_empty()
        }) {
            return Err("financial-report event has an empty instrument or publication key".into());
        }
        let mut sorted = events.iter().collect::<Vec<_>>();
        sorted.sort_by(|a, b| {
            a.publication_date
                .cmp(&b.publication_date)
                .then_with(|| a.instrument_id.cmp(&b.instrument_id))
                .then_with(|| a.publication_key.cmp(&b.publication_key))
        });
        sorted.dedup_by(|right, left| {
            left.instrument_id == right.instrument_id
                && left.publication_key == right.publication_key
        });
        let mut histories: BTreeMap<String, Vec<&DailyBar>> = BTreeMap::new();
        for bar in bars {
            histories
                .entry(bar.instrument_id.clone())
                .or_default()
                .push(bar);
        }
        for history in histories.values_mut() {
            history.sort_by_key(|bar| bar.date);
        }
        Ok(Self {
            first_publication_date: sorted[0].publication_date,
            events: sorted,
            next: 0,
            by_instrument: BTreeMap::new(),
            histories,
        })
    }

    fn advance_before(&mut self, decision_date: Date) {
        while self.next < self.events.len()
            && self.events[self.next].publication_date < decision_date
        {
            let event = self.events[self.next];
            self.by_instrument
                .entry(event.instrument_id.clone())
                .or_default()
                .push(event);
            self.next += 1;
        }
    }

    fn values(&self, instrument_id: &str, decision_date: Date) -> Vec<Option<f64>> {
        if decision_date <= self.first_publication_date {
            return vec![None; REPORT_EVENT_FEATURE_NAMES.len()];
        }
        let Some(events) = self.by_instrument.get(instrument_id) else {
            return vec![None, Some(0.0), Some(0.0), None, None, None];
        };
        let latest = events
            .last()
            .expect("financial-report instrument contains an event");
        let since_30 = decision_date - time::Duration::days(30);
        let since_90 = decision_date - time::Duration::days(90);
        let history = self.histories.get(instrument_id);
        vec![
            Some((decision_date - latest.publication_date).whole_days() as f64),
            Some(
                events
                    .iter()
                    .filter(|event| event.publication_date >= since_30)
                    .count() as f64,
            ),
            Some(
                events
                    .iter()
                    .filter(|event| event.publication_date >= since_90)
                    .count() as f64,
            ),
            history.and_then(|history| report_reaction(history, latest, decision_date, 1)),
            history.and_then(|history| report_reaction(history, latest, decision_date, 5)),
            history.and_then(|history| report_reaction(history, latest, decision_date, 21)),
        ]
    }
}

fn report_reaction(
    history: &[&DailyBar],
    event: &FinancialReportEvent,
    decision_date: Date,
    horizon_sessions: usize,
) -> Option<f64> {
    let baseline_end = if event.after_market_close {
        history.partition_point(|bar| bar.date <= event.publication_date)
    } else {
        history.partition_point(|bar| bar.date < event.publication_date)
    };
    let baseline_index = baseline_end.checked_sub(1)?;
    let current_end = history.partition_point(|bar| bar.date <= decision_date);
    let current_index = current_end.checked_sub(1)?;
    if current_index <= baseline_index {
        return None;
    }
    let reaction_index = baseline_index
        .checked_add(horizon_sessions)?
        .min(current_index);
    finite(history[reaction_index].adjusted_close / history[baseline_index].adjusted_close - 1.0)
}

/// Provider-neutral annual statement event. Provider taxonomy decoding and
/// issuer mapping must be completed before this boundary; feature ratios and
/// the decision-date price join remain owned by this crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnualFundamentalEvent {
    pub instrument_id: String,
    #[serde(with = "date_serde")]
    pub available_date: Date,
    #[serde(with = "date_serde")]
    pub report_period_end: Date,
    pub filing_key: String,
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

struct AnnualFundamentalCursor<'a> {
    events: Vec<&'a AnnualFundamentalEvent>,
    next: usize,
    first_available_date: Date,
    by_instrument: BTreeMap<String, Vec<&'a AnnualFundamentalEvent>>,
    histories: BTreeMap<String, Vec<&'a DailyBar>>,
}

impl<'a> AnnualFundamentalCursor<'a> {
    fn new(events: &'a [AnnualFundamentalEvent], bars: &'a [DailyBar]) -> Result<Self, String> {
        if events.is_empty() {
            return Err("annual-fundamental feature set requires events".into());
        }
        for event in events {
            if event.instrument_id.trim().is_empty()
                || event.filing_key.trim().is_empty()
                || event.available_date <= event.report_period_end
            {
                return Err("annual-fundamental event has invalid identity or dates".into());
            }
            let values = [
                event.revenue,
                event.prior_revenue,
                event.operating_profit,
                event.net_income,
                event.prior_net_income,
                event.assets,
                event.prior_assets,
                event.equity,
                event.prior_equity,
                event.cash,
                event.operating_cash_flow,
                event.current_assets,
                event.current_liabilities,
                event.basic_eps,
                event.weighted_average_shares,
            ];
            if values.into_iter().flatten().any(|value| !value.is_finite()) {
                return Err("annual-fundamental event has a non-finite value".into());
            }
        }
        let mut sorted = events.iter().collect::<Vec<_>>();
        sorted.sort_by(|left, right| {
            left.available_date
                .cmp(&right.available_date)
                .then_with(|| left.instrument_id.cmp(&right.instrument_id))
                .then_with(|| left.report_period_end.cmp(&right.report_period_end))
                .then_with(|| left.filing_key.cmp(&right.filing_key))
        });
        sorted.dedup_by(|right, left| {
            left.instrument_id == right.instrument_id && left.filing_key == right.filing_key
        });
        let mut histories: BTreeMap<String, Vec<&DailyBar>> = BTreeMap::new();
        for bar in bars {
            histories
                .entry(bar.instrument_id.clone())
                .or_default()
                .push(bar);
        }
        for history in histories.values_mut() {
            history.sort_by_key(|bar| bar.date);
        }
        Ok(Self {
            first_available_date: sorted[0].available_date,
            events: sorted,
            next: 0,
            by_instrument: BTreeMap::new(),
            histories,
        })
    }

    fn advance_before(&mut self, decision_date: Date) {
        while self.next < self.events.len() && self.events[self.next].available_date < decision_date
        {
            let event = self.events[self.next];
            self.by_instrument
                .entry(event.instrument_id.clone())
                .or_default()
                .push(event);
            self.next += 1;
        }
    }

    fn values(&self, instrument_id: &str, decision_date: Date) -> Vec<Option<f64>> {
        if decision_date <= self.first_available_date {
            return vec![None; FUNDAMENTAL_FEATURE_NAMES.len()];
        }
        let Some(event) = self.by_instrument.get(instrument_id).and_then(|events| {
            events.iter().copied().max_by(|left, right| {
                left.report_period_end
                    .cmp(&right.report_period_end)
                    .then_with(|| left.available_date.cmp(&right.available_date))
                    .then_with(|| left.filing_key.cmp(&right.filing_key))
            })
        }) else {
            return vec![None; FUNDAMENTAL_FEATURE_NAMES.len()];
        };
        let price = self.histories.get(instrument_id).and_then(|history| {
            let end = history.partition_point(|bar| bar.date <= decision_date);
            end.checked_sub(1).map(|index| history[index].raw_close)
        });
        let market_value =
            price.and_then(|price| safe_ratio(price * event.weighted_average_shares?, 1.0));
        let sek = event
            .reporting_currency
            .as_deref()
            .is_some_and(|currency| currency == "iso4217:SEK");
        vec![
            Some((decision_date - event.available_date).whole_days() as f64),
            ratio(event.equity, event.assets),
            ratio(event.cash, event.assets),
            ratio(event.operating_cash_flow, event.assets),
            ratio(
                event
                    .net_income
                    .zip(event.operating_cash_flow)
                    .map(|(income, cash)| income - cash),
                event.assets,
            ),
            ratio(event.operating_profit, event.revenue),
            ratio(event.net_income, event.revenue),
            growth(event.revenue, event.prior_revenue),
            ratio(
                event
                    .net_income
                    .zip(event.prior_net_income)
                    .map(|(current, prior)| current - prior),
                event.assets,
            ),
            growth(event.assets, event.prior_assets),
            growth(event.equity, event.prior_equity),
            ratio(event.current_assets, event.current_liabilities),
            sek.then(|| ratio(event.basic_eps, price)).flatten(),
            sek.then(|| ratio(event.equity, market_value)).flatten(),
            sek.then(|| ratio(event.revenue, market_value)).flatten(),
        ]
    }
}

fn ratio(numerator: Option<f64>, denominator: Option<f64>) -> Option<f64> {
    safe_ratio(numerator?, denominator?)
}

fn safe_ratio(numerator: f64, denominator: f64) -> Option<f64> {
    (numerator.is_finite() && denominator.is_finite() && denominator > 0.0)
        .then(|| numerator / denominator)
        .filter(|value| value.is_finite())
}

fn growth(current: Option<f64>, prior: Option<f64>) -> Option<f64> {
    safe_ratio(current?, prior?).and_then(|ratio| finite(ratio - 1.0))
}

/// Produce the complete model input contract for one decision date. This is
/// shared by matrix construction and future live inference: all inputs are
/// trailing per-instrument values or aggregates of the contemporaneous
/// decision-date cross-section.
pub fn model_cross_section(inputs: &[CrossSectionInput]) -> Result<Vec<NormalisedRow>, String> {
    let Some(date) = inputs.first().map(|input| input.feature.date) else {
        return Ok(Vec::new());
    };
    if inputs.iter().any(|input| {
        input.feature.date != date
            || input.feature.instrument_id != input.meta.instrument_id
            || input.feature.values.len() != FEATURE_NAMES.len()
    }) {
        return Err("invalid Stockholm contextual cross-section".into());
    }
    let source_indexes = SECTOR_RELATIVE_SOURCES
        .iter()
        .map(|name| feature_index(name))
        .collect::<Result<Vec<_>, _>>()?;
    let mut sector_values: BTreeMap<(&str, usize), Vec<f64>> = BTreeMap::new();
    for input in inputs {
        for source in &source_indexes {
            if let Some(value) = input.feature.values[*source].filter(|value| value.is_finite()) {
                sector_values
                    .entry((input.meta.sector.as_str(), *source))
                    .or_default()
                    .push(value);
            }
        }
    }
    let sector_medians = sector_values
        .into_iter()
        .filter_map(|(key, mut values)| {
            (values.len() >= 3)
                .then(|| median(&mut values).map(|value| (key, value)))
                .flatten()
        })
        .collect::<BTreeMap<_, _>>();
    let sector_relative = inputs
        .iter()
        .map(|input| {
            source_indexes
                .iter()
                .map(|source| {
                    let value = input.feature.values[*source]?;
                    let sector = sector_medians.get(&(input.meta.sector.as_str(), *source))?;
                    finite(value - sector)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let market = market_context(inputs)?;
    let mut by_bucket: BTreeMap<UniverseBucket, Vec<usize>> = BTreeMap::new();
    for (index, input) in inputs.iter().enumerate() {
        by_bucket
            .entry(input.meta.bucket.clone())
            .or_default()
            .push(index);
    }
    let value_width = FEATURE_NAMES.len() + CONTEXT_FEATURE_NAMES.len();
    let mut output = vec![vec![0.0; value_width * 2]; inputs.len()];
    for indexes in by_bucket.values() {
        let base_raw = indexes
            .iter()
            .map(|index| inputs[*index].feature.values.clone())
            .collect::<Vec<_>>();
        let relative_raw = indexes
            .iter()
            .map(|index| sector_relative[*index].clone())
            .collect::<Vec<_>>();
        let base_ranked = features_common::rank_normalise(&base_raw)?;
        let relative_ranked = features_common::rank_normalise(&relative_raw)?;
        for ((index, base), relative) in indexes.iter().zip(base_ranked).zip(relative_ranked) {
            let mut values = base;
            values.extend(relative);
            values.extend(market.iter().map(|value| value.unwrap_or(0.0)));
            let mut missing = inputs[*index]
                .feature
                .values
                .iter()
                .map(|value| f64::from(value.is_none()))
                .collect::<Vec<_>>();
            missing.extend(
                sector_relative[*index]
                    .iter()
                    .map(|value| f64::from(value.is_none())),
            );
            missing.extend(market.iter().map(|value| f64::from(value.is_none())));
            values.extend(missing);
            output[*index] = values;
        }
    }
    Ok(inputs
        .iter()
        .zip(output)
        .map(|(input, values)| NormalisedRow {
            date,
            instrument_id: input.meta.instrument_id.clone(),
            values,
        })
        .collect())
}

fn baseline_cross_section(inputs: &[CrossSectionInput]) -> Result<Vec<NormalisedRow>, String> {
    let Some(date) = inputs.first().map(|input| input.feature.date) else {
        return Ok(Vec::new());
    };
    let mut by_bucket: BTreeMap<UniverseBucket, Vec<usize>> = BTreeMap::new();
    for (index, input) in inputs.iter().enumerate() {
        by_bucket
            .entry(input.meta.bucket.clone())
            .or_default()
            .push(index);
    }
    let mut output = vec![Vec::new(); inputs.len()];
    for indexes in by_bucket.values() {
        let raw = indexes
            .iter()
            .map(|index| inputs[*index].feature.values.clone())
            .collect::<Vec<_>>();
        for (index, mut values) in indexes.iter().zip(features_common::rank_normalise(&raw)?) {
            values.extend(
                inputs[*index]
                    .feature
                    .values
                    .iter()
                    .map(|value| f64::from(value.is_none())),
            );
            output[*index] = values;
        }
    }
    Ok(inputs
        .iter()
        .zip(output)
        .map(|(input, values)| NormalisedRow {
            date,
            instrument_id: input.meta.instrument_id.clone(),
            values,
        })
        .collect())
}

fn baseline_global_risk_cross_section(
    inputs: &[CrossSectionInput],
    global: &[Option<f64>],
) -> Result<Vec<NormalisedRow>, String> {
    if global.len() != GLOBAL_RISK_FEATURE_NAMES.len() {
        return Err("invalid global-risk feature row".into());
    }
    let mut rows = baseline_cross_section(inputs)?;
    for row in &mut rows {
        row.values
            .extend(global.iter().map(|value| value.unwrap_or(0.0)));
        row.values
            .extend(global.iter().map(|value| f64::from(value.is_none())));
    }
    Ok(rows)
}

fn global_risk_features(
    observations: &[GlobalRiskBar],
) -> Result<BTreeMap<Date, Vec<Option<f64>>>, String> {
    if observations.is_empty() {
        return Ok(BTreeMap::new());
    }
    if observations
        .windows(2)
        .any(|pair| pair[0].date >= pair[1].date)
        || observations
            .iter()
            .any(|bar| !bar.close.is_finite() || bar.close <= 0.0)
    {
        return Err("global-risk history must be valid, strictly increasing daily closes".into());
    }
    let mut output = BTreeMap::new();
    for index in 0..observations.len() {
        let simple_return = |window: usize| {
            let start = index.checked_sub(window)?;
            finite(observations[index].close / observations[start].close - 1.0)
        };
        let volatility = |window: usize| {
            let start = index.checked_sub(window)?;
            let returns = (start + 1..=index)
                .map(|slot| finite((observations[slot].close / observations[slot - 1].close).ln()))
                .collect::<Option<Vec<_>>>()?;
            finite(standard_deviation(&returns)? * 252.0_f64.sqrt())
        };
        let drawdown = index.checked_add(1).and_then(|end| {
            let start = end.checked_sub(126)?;
            let mut peak = observations[start].close;
            let mut worst = 0.0_f64;
            for bar in &observations[start..=index] {
                peak = peak.max(bar.close);
                worst = worst.min(bar.close / peak - 1.0);
            }
            finite(worst)
        });
        let values = vec![
            simple_return(1),
            simple_return(5),
            simple_return(21),
            simple_return(63),
            simple_return(126),
            volatility(20),
            volatility(60),
            drawdown,
        ];
        debug_assert_eq!(values.len(), GLOBAL_RISK_FEATURE_NAMES.len());
        output.insert(observations[index].date, values);
    }
    Ok(output)
}

fn global_risk_before(features: &BTreeMap<Date, Vec<Option<f64>>>, date: Date) -> Vec<Option<f64>> {
    features
        .range(..date)
        .next_back()
        .map(|(_, values)| values.clone())
        .unwrap_or_else(|| vec![None; GLOBAL_RISK_FEATURE_NAMES.len()])
}

/// Cross-asset futures context observed at the same day's Stockholm close.
/// Unlike `global_risk_before`, exact-date values are admissible because the
/// shared CME reader emits only bars completed by 17:30 Europe/Stockholm and
/// the equity decision is executed no earlier than the following session.
fn stockholm_close_global_risk_features(
    series: &[GlobalRiskSeries],
) -> Result<BTreeMap<Date, Vec<Option<f64>>>, String> {
    let by_symbol = series
        .iter()
        .map(|history| (history.symbol.as_str(), history))
        .collect::<BTreeMap<_, _>>();
    if by_symbol.len() != series.len() {
        return Err("duplicate Stockholm-close global-risk symbol".into());
    }
    for symbol in STOCKHOLM_CLOSE_GLOBAL_RISK_SYMBOLS {
        if !by_symbol.contains_key(symbol) {
            return Err(format!(
                "Stockholm-close global-risk series {symbol} is required"
            ));
        }
    }
    let computed = STOCKHOLM_CLOSE_GLOBAL_RISK_SYMBOLS
        .iter()
        .map(|symbol| {
            global_risk_features(&by_symbol[symbol].observations)
                .map(|values| ((*symbol).to_owned(), values))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let dates = computed
        .values()
        .flat_map(|values| values.keys().copied())
        .collect::<BTreeSet<_>>();
    Ok(dates
        .into_iter()
        .map(|date| {
            let values = STOCKHOLM_CLOSE_GLOBAL_RISK_SYMBOLS
                .iter()
                .flat_map(|symbol| {
                    computed[*symbol]
                        .get(&date)
                        .cloned()
                        .unwrap_or_else(|| vec![None; GLOBAL_RISK_FEATURE_NAMES.len()])
                })
                .collect::<Vec<_>>();
            (date, values)
        })
        .collect())
}

fn append_stockholm_close_global_risk(
    mut rows: Vec<NormalisedRow>,
    global: &[Option<f64>],
) -> Result<Vec<NormalisedRow>, String> {
    let expected = stockholm_close_global_risk_feature_names().len();
    if global.len() != expected {
        return Err("invalid Stockholm-close global-risk feature row".into());
    }
    for row in &mut rows {
        row.values
            .extend(global.iter().map(|value| value.unwrap_or(0.0)));
        row.values
            .extend(global.iter().map(|value| f64::from(value.is_none())));
    }
    Ok(rows)
}

/// Build trailing residual-risk inputs from the complete eligible universe.
/// All factor returns on date `t` use adjusted closes no later than `t`.
fn residual_features(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    eligible_from: &BTreeMap<String, Date>,
) -> Result<BTreeMap<(String, Date), ResidualFeatureRow>, String> {
    let meta = instruments
        .iter()
        .filter(|instrument| instrument.bucket.is_stockholm_main_market())
        .map(|instrument| (instrument.instrument_id.as_str(), instrument))
        .collect::<BTreeMap<_, _>>();
    let mut grouped: BTreeMap<&str, Vec<&DailyBar>> = BTreeMap::new();
    for bar in bars {
        if meta.contains_key(bar.instrument_id.as_str()) {
            grouped.entry(&bar.instrument_id).or_default().push(bar);
        }
    }
    for history in grouped.values_mut() {
        history.sort_by_key(|bar| bar.date);
    }

    let mut market_members: BTreeMap<Date, Vec<f64>> = BTreeMap::new();
    let mut sector_members: BTreeMap<(String, Date), Vec<f64>> = BTreeMap::new();
    for (instrument_id, history) in &grouped {
        let instrument = meta[instrument_id];
        for pair in history.windows(2) {
            if eligible_from
                .get(*instrument_id)
                .is_some_and(|admission| pair[1].date < *admission)
            {
                continue;
            }
            let value = pair[1].adjusted_close / pair[0].adjusted_close - 1.0;
            if value.is_finite() && value > -1.0 {
                market_members.entry(pair[1].date).or_default().push(value);
                sector_members
                    .entry((instrument.sector.clone(), pair[1].date))
                    .or_default()
                    .push(value);
            }
        }
    }
    let market = market_members
        .into_iter()
        .filter_map(|(date, values)| {
            (values.len() >= 20)
                .then(|| finite(values.iter().sum::<f64>() / values.len() as f64))
                .flatten()
                .map(|value| (date, value))
        })
        .collect::<BTreeMap<_, _>>();
    let sector = sector_members
        .into_iter()
        .filter_map(|(key, values)| {
            (values.len() >= 3)
                .then(|| finite(values.iter().sum::<f64>() / values.len() as f64))
                .flatten()
                .map(|value| (key, value))
        })
        .collect::<BTreeMap<_, _>>();

    let mut output = BTreeMap::new();
    for (instrument_id, history) in grouped {
        let instrument = meta[instrument_id];
        for index in 0..history.len() {
            let beta_rows =
                aligned_factor_returns(&history, index, 252, &instrument.sector, &market, &sector);
            let beta_252 = beta_rows.as_deref().and_then(market_beta);
            let idio_vol_60 =
                aligned_factor_returns(&history, index, 60, &instrument.sector, &market, &sector)
                    .as_deref()
                    .and_then(two_factor_residual_volatility);
            let log_adv_sek_20 = median_notional(&history, index, 20)
                .filter(|value| *value > 0.0)
                .and_then(|value| finite(value.ln()));
            let log_adv_sek_60 = median_notional(&history, index, 60)
                .filter(|value| *value > 0.0)
                .and_then(|value| finite(value.ln()));
            let range_frac_14 = trailing_mean(&history, index, 14, |bar| {
                finite((bar.raw_high - bar.raw_low) / bar.raw_close)
            });
            let close_location_5 = trailing_mean(&history, index, 5, close_location);
            let market_return_21 =
                factor_return(&history, index, 21, |date| market.get(&date).copied());
            let market_return_126 =
                factor_return(&history, index, 126, |date| market.get(&date).copied());
            let sector_return_21 = factor_return(&history, index, 21, |date| {
                sector.get(&(instrument.sector.clone(), date)).copied()
            });
            let sector_return_126 = factor_return(&history, index, 126, |date| {
                sector.get(&(instrument.sector.clone(), date)).copied()
            });
            let stock_return_21 = simple_return(&history, index, 21);
            let stock_return_126 = simple_return(&history, index, 126);
            let values = vec![
                beta_252,
                idio_vol_60,
                log_adv_sek_20,
                log_adv_sek_60,
                amihud(&history, index, 60),
                range_frac_14,
                close_location_5,
                residual_return(stock_return_21, market_return_21, beta_252),
                residual_return(stock_return_126, market_return_126, beta_252),
                relative_return(stock_return_21, sector_return_21),
                relative_return(stock_return_126, sector_return_126),
            ];
            debug_assert_eq!(values.len(), RESIDUAL_FEATURE_NAMES.len());
            let row = ResidualFeatureRow {
                date: history[index].date,
                instrument_id: instrument_id.to_owned(),
                values,
            };
            output.insert((instrument_id.to_owned(), row.date), row);
        }
    }
    Ok(output)
}

fn macro_features(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    series: &[MacroSeries],
) -> Result<ExternalFeatureRows, String> {
    let mut by_id = BTreeMap::new();
    for input in series {
        if by_id.insert(input.series_id.as_str(), input).is_some() {
            return Err(format!("duplicate macro series {:?}", input.series_id));
        }
        if input.observations.is_empty()
            || input
                .observations
                .iter()
                .any(|observation| !observation.value.is_finite())
            || input
                .observations
                .windows(2)
                .any(|pair| pair[0].date >= pair[1].date)
        {
            return Err(format!("invalid macro series {:?}", input.series_id));
        }
    }
    let required = REQUIRED_MACRO_SERIES
        .iter()
        .map(|id| {
            let series = by_id
                .get(id)
                .copied()
                .ok_or_else(|| format!("missing required macro series {id}"))?;
            if series
                .observations
                .iter()
                .any(|observation| observation.value <= 0.0)
            {
                return Err(format!(
                    "required log-return macro series {id} contains a non-positive value"
                ));
            }
            Ok(series)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let main_market = instruments
        .iter()
        .filter(|instrument| instrument.bucket.is_stockholm_main_market())
        .map(|instrument| instrument.instrument_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut grouped: BTreeMap<&str, Vec<&DailyBar>> = BTreeMap::new();
    for bar in bars {
        if main_market.contains(bar.instrument_id.as_str()) {
            grouped.entry(&bar.instrument_id).or_default().push(bar);
        }
    }
    let mut output = BTreeMap::new();
    for (instrument_id, mut history) in grouped {
        history.sort_by_key(|bar| bar.date);
        let levels = history
            .iter()
            .map(|bar| {
                required
                    .iter()
                    .map(|series| macro_level_on_or_before(&series.observations, bar.date))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for index in 0..history.len() {
            let betas = (0..required.len())
                .map(|series_index| macro_beta(&history, &levels, index, series_index, 126))
                .collect::<Vec<_>>();
            let mut values = betas.clone();
            for window in [21, 63] {
                for (series_index, beta) in betas.iter().copied().enumerate() {
                    values.push(
                        beta.zip(macro_return(&levels, index, series_index, window))
                            .and_then(|(beta, change)| finite(beta * change)),
                    );
                }
            }
            debug_assert_eq!(values.len(), MACRO_FEATURE_NAMES.len());
            output.insert((instrument_id.to_owned(), history[index].date), values);
        }
    }
    Ok(output)
}

#[derive(Debug)]
struct MicrostructureFeatureRows {
    features: ExternalFeatureRows,
    median_spread_bps_20: BTreeMap<(String, Date), f64>,
}

fn microstructure_features(
    observations: &[MarketMicrostructureBar],
) -> Result<MicrostructureFeatureRows, String> {
    let mut grouped: BTreeMap<&str, Vec<&MarketMicrostructureBar>> = BTreeMap::new();
    for observation in observations {
        if observation.instrument_id.trim().is_empty()
            || !observation.close.is_finite()
            || observation.close <= 0.0
            || !observation.turnover_sek.is_finite()
            || observation.turnover_sek < 0.0
            || observation
                .bid
                .is_some_and(|value| !value.is_finite() || value <= 0.0)
            || observation
                .ask
                .is_some_and(|value| !value.is_finite() || value <= 0.0)
            || observation
                .average
                .is_some_and(|value| !value.is_finite() || value <= 0.0)
            || observation
                .bid
                .zip(observation.ask)
                .is_some_and(|(bid, ask)| bid > ask)
        {
            return Err(format!(
                "invalid market microstructure observation for {} on {}",
                observation.instrument_id, observation.date
            ));
        }
        grouped
            .entry(&observation.instrument_id)
            .or_default()
            .push(observation);
    }

    let mut output = BTreeMap::new();
    let mut median_spread_bps_20 = BTreeMap::new();
    for (instrument_id, mut history) in grouped {
        history.sort_by_key(|bar| bar.date);
        if history.windows(2).any(|pair| pair[0].date >= pair[1].date) {
            return Err(format!(
                "market microstructure history for {instrument_id} is not unique"
            ));
        }
        for index in 0..history.len() {
            let current_spread = closing_spread_bps(history[index]);
            let window_start = index.saturating_add(1).saturating_sub(20);
            let window = &history[window_start..=index];
            let mut spreads = window
                .iter()
                .filter_map(|bar| closing_spread_bps(bar))
                .collect::<Vec<_>>();
            let median_spread = (spreads.len() >= 10)
                .then(|| median(&mut spreads))
                .flatten();
            let spread_ratio = current_spread
                .zip(median_spread)
                .and_then(|(current, baseline)| safe_ratio(current, baseline));
            let mut trades = window
                .iter()
                .filter_map(|bar| bar.trades.map(|value| value as f64))
                .filter(|value| *value > 0.0)
                .collect::<Vec<_>>();
            let median_trades = (trades.len() >= 10).then(|| median(&mut trades)).flatten();
            let log_median_trades = median_trades.and_then(|value| finite(value.ln()));
            let trades_surge = history[index]
                .trades
                .map(|value| value as f64)
                .zip(median_trades)
                .and_then(|(current, baseline)| safe_ratio(current, baseline));
            let close_vs_average = history[index]
                .average
                .and_then(|average| safe_ratio(history[index].close, average))
                .and_then(|ratio| finite(ratio - 1.0));
            let mut trade_values = window
                .iter()
                .filter_map(|bar| {
                    let trades = bar.trades? as f64;
                    (trades > 0.0 && bar.turnover_sek > 0.0).then(|| bar.turnover_sek / trades)
                })
                .collect::<Vec<_>>();
            let log_median_trade_value = (trade_values.len() >= 10)
                .then(|| median(&mut trade_values))
                .flatten()
                .filter(|value| *value > 0.0)
                .and_then(|value| finite(value.ln()));
            let values = vec![
                current_spread,
                median_spread,
                spread_ratio,
                log_median_trades,
                trades_surge,
                close_vs_average,
                log_median_trade_value,
            ];
            debug_assert_eq!(values.len(), MICROSTRUCTURE_FEATURE_NAMES.len());
            let key = (instrument_id.to_owned(), history[index].date);
            if let Some(spread) = median_spread {
                median_spread_bps_20.insert(key.clone(), spread);
            }
            output.insert(key, values);
        }
    }
    Ok(MicrostructureFeatureRows {
        features: output,
        median_spread_bps_20,
    })
}

fn closing_spread_bps(bar: &MarketMicrostructureBar) -> Option<f64> {
    let (bid, ask) = bar.bid.zip(bar.ask)?;
    let midpoint = (bid + ask) / 2.0;
    (ask >= bid && midpoint > 0.0)
        .then(|| (ask - bid) / midpoint * 10_000.0)
        .and_then(finite)
}

fn borrow_fee_features(observations: &[BorrowFeeBar]) -> Result<ExternalFeatureRows, String> {
    let mut grouped: BTreeMap<&str, Vec<&BorrowFeeBar>> = BTreeMap::new();
    for observation in observations {
        if observation.instrument_id.trim().is_empty()
            || !observation.annual_rate.is_finite()
            || observation.annual_rate < 0.0
        {
            return Err(format!(
                "invalid borrow-fee observation for {} on {}",
                observation.instrument_id, observation.date
            ));
        }
        grouped
            .entry(&observation.instrument_id)
            .or_default()
            .push(observation);
    }

    let mut output = BTreeMap::new();
    for (instrument_id, mut history) in grouped {
        history.sort_by_key(|bar| bar.date);
        if history.windows(2).any(|pair| pair[0].date >= pair[1].date) {
            return Err(format!(
                "borrow-fee history for {instrument_id} is not unique"
            ));
        }
        for index in 0..history.len() {
            let current = history[index].annual_rate;
            let start = index.saturating_add(1).saturating_sub(20);
            let mut window = history[start..=index]
                .iter()
                .map(|bar| bar.annual_rate)
                .collect::<Vec<_>>();
            let median_20 = (window.len() >= 10).then(|| median(&mut window)).flatten();
            let change_5 = index
                .checked_sub(5)
                .and_then(|prior| finite(current - history[prior].annual_rate));
            let change_20 = index
                .checked_sub(20)
                .and_then(|prior| finite(current - history[prior].annual_rate));
            let max_20 = (window.len() >= 10)
                .then(|| window.into_iter().reduce(f64::max))
                .flatten();
            let values = vec![Some(current), median_20, change_5, change_20, max_20];
            debug_assert_eq!(values.len(), BORROW_FEE_FEATURE_NAMES.len());
            output.insert((instrument_id.to_owned(), history[index].date), values);
        }
    }
    Ok(output)
}

fn macro_level_on_or_before(observations: &[MacroObservation], date: Date) -> Option<f64> {
    let end = observations.partition_point(|observation| observation.date <= date);
    end.checked_sub(1).map(|index| observations[index].value)
}

fn macro_return(
    levels: &[Vec<Option<f64>>],
    index: usize,
    series_index: usize,
    window: usize,
) -> Option<f64> {
    let start = index.checked_sub(window)?;
    let current = levels[index][series_index]?;
    let prior = levels[start][series_index]?;
    finite(current / prior - 1.0)
}

fn macro_beta(
    history: &[&DailyBar],
    levels: &[Vec<Option<f64>>],
    index: usize,
    series_index: usize,
    window: usize,
) -> Option<f64> {
    let start = index.checked_sub(window)?;
    let rows = (start + 1..=index)
        .filter_map(|current| {
            let macro_current = levels[current][series_index]?;
            let macro_prior = levels[current - 1][series_index]?;
            let macro_return = (macro_current / macro_prior).ln();
            let stock_return =
                (history[current].adjusted_close / history[current - 1].adjusted_close).ln();
            (macro_return.is_finite() && stock_return.is_finite())
                .then_some((macro_return, stock_return))
        })
        .collect::<Vec<_>>();
    if rows.len() < 80 {
        return None;
    }
    let macro_mean = rows.iter().map(|row| row.0).sum::<f64>() / rows.len() as f64;
    let stock_mean = rows.iter().map(|row| row.1).sum::<f64>() / rows.len() as f64;
    let variance = rows
        .iter()
        .map(|row| (row.0 - macro_mean).powi(2))
        .sum::<f64>();
    if variance <= f64::EPSILON {
        return None;
    }
    finite(
        rows.iter()
            .map(|row| (row.0 - macro_mean) * (row.1 - stock_mean))
            .sum::<f64>()
            / variance,
    )
}

/// (stock log return, market log return, sector-minus-market log return).
fn aligned_factor_returns(
    history: &[&DailyBar],
    index: usize,
    window: usize,
    sector_name: &str,
    market: &BTreeMap<Date, f64>,
    sectors: &BTreeMap<(String, Date), f64>,
) -> Option<Vec<(f64, f64, f64)>> {
    let start = index.checked_sub(window)?;
    let rows = (start + 1..=index)
        .map(|current| {
            let date = history[current].date;
            let stock =
                (history[current].adjusted_close / history[current - 1].adjusted_close).ln();
            let market = market.get(&date)?.ln_1p();
            let sector = sectors.get(&(sector_name.to_owned(), date))?.ln_1p();
            (stock.is_finite() && market.is_finite() && sector.is_finite()).then_some((
                stock,
                market,
                sector - market,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    (rows.len() == window).then_some(rows)
}

fn market_beta(rows: &[(f64, f64, f64)]) -> Option<f64> {
    let stock_mean = rows.iter().map(|row| row.0).sum::<f64>() / rows.len() as f64;
    let market_mean = rows.iter().map(|row| row.1).sum::<f64>() / rows.len() as f64;
    let covariance = rows
        .iter()
        .map(|row| (row.0 - stock_mean) * (row.1 - market_mean))
        .sum::<f64>();
    let variance = rows
        .iter()
        .map(|row| (row.1 - market_mean).powi(2))
        .sum::<f64>();
    (variance > f64::EPSILON)
        .then(|| finite(covariance / variance))
        .flatten()
}

fn two_factor_residual_volatility(rows: &[(f64, f64, f64)]) -> Option<f64> {
    if rows.len() < 3 {
        return None;
    }
    let means = (
        rows.iter().map(|row| row.0).sum::<f64>() / rows.len() as f64,
        rows.iter().map(|row| row.1).sum::<f64>() / rows.len() as f64,
        rows.iter().map(|row| row.2).sum::<f64>() / rows.len() as f64,
    );
    let centered = rows
        .iter()
        .map(|row| (row.0 - means.0, row.1 - means.1, row.2 - means.2))
        .collect::<Vec<_>>();
    let mm = centered.iter().map(|row| row.1 * row.1).sum::<f64>();
    let ss = centered.iter().map(|row| row.2 * row.2).sum::<f64>();
    let ms = centered.iter().map(|row| row.1 * row.2).sum::<f64>();
    let ym = centered.iter().map(|row| row.0 * row.1).sum::<f64>();
    let ys = centered.iter().map(|row| row.0 * row.2).sum::<f64>();
    let determinant = mm * ss - ms * ms;
    if determinant <= f64::EPSILON {
        return None;
    }
    let beta_market = (ym * ss - ys * ms) / determinant;
    let beta_sector = (ys * mm - ym * ms) / determinant;
    standard_deviation(
        &centered
            .iter()
            .map(|row| row.0 - beta_market * row.1 - beta_sector * row.2)
            .collect::<Vec<_>>(),
    )
}

fn trailing_mean<F>(history: &[&DailyBar], index: usize, window: usize, value: F) -> Option<f64>
where
    F: Fn(&DailyBar) -> Option<f64>,
{
    let values = trailing(history, index, window)?
        .iter()
        .map(|bar| value(bar))
        .collect::<Option<Vec<_>>>()?;
    finite(values.iter().sum::<f64>() / values.len() as f64)
}

fn factor_return<F>(history: &[&DailyBar], index: usize, window: usize, value: F) -> Option<f64>
where
    F: Fn(Date) -> Option<f64>,
{
    let start = index.checked_sub(window)?;
    let total = (start + 1..=index)
        .map(|current| value(history[current].date).and_then(|value| finite(value.ln_1p())))
        .collect::<Option<Vec<_>>>()?
        .iter()
        .sum::<f64>();
    finite(total.exp() - 1.0)
}

fn residual_return(stock: Option<f64>, market: Option<f64>, beta: Option<f64>) -> Option<f64> {
    finite(stock? - beta? * market?)
}

fn relative_return(stock: Option<f64>, sector: Option<f64>) -> Option<f64> {
    finite(stock? - sector?)
}

fn market_context(inputs: &[CrossSectionInput]) -> Result<Vec<Option<f64>>, String> {
    let mut output = Vec::new();
    for name in MARKET_MEDIAN_SOURCES {
        let index = feature_index(name)?;
        let mut values = inputs
            .iter()
            .filter_map(|input| input.feature.values[index])
            .collect::<Vec<_>>();
        output.push((values.len() >= 20).then(|| median(&mut values)).flatten());
    }
    let ret21 = measured_feature(inputs, "ret_21")?;
    output.push(
        (ret21.len() >= 20)
            .then(|| standard_deviation(&ret21))
            .flatten(),
    );
    output.push(positive_fraction(&measured_feature(inputs, "ret_21")?));
    output.push(positive_fraction(&measured_feature(inputs, "ret_126")?));
    let mut vol20 = measured_feature(inputs, "vol_20")?;
    output.push((vol20.len() >= 20).then(|| median(&mut vol20)).flatten());
    debug_assert_eq!(
        output.len(),
        CONTEXT_FEATURE_NAMES.len() - source_indexes_len()
    );
    Ok(output)
}

fn source_indexes_len() -> usize {
    SECTOR_RELATIVE_SOURCES.len()
}

fn measured_feature(inputs: &[CrossSectionInput], name: &str) -> Result<Vec<f64>, String> {
    let index = feature_index(name)?;
    Ok(inputs
        .iter()
        .filter_map(|input| input.feature.values[index].filter(|value| value.is_finite()))
        .collect())
}

fn positive_fraction(values: &[f64]) -> Option<f64> {
    if values.len() < 20 {
        None
    } else {
        finite(values.iter().filter(|value| **value > 0.0).count() as f64 / values.len() as f64)
    }
}

fn feature_index(name: &str) -> Result<usize, String> {
    FEATURE_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .ok_or_else(|| format!("unknown base feature {name:?}"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrainingRow {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub instrument_id: String,
    pub symbol: String,
    pub isin: String,
    pub sector: String,
    pub bucket: UniverseBucket,
    /// Adjusted-close return from `t-252` through `t-21`. This Rust-owned raw
    /// value supports the predeclared fixed 12-1 momentum baseline; it is not
    /// silently added to any existing model feature contract.
    #[serde(default)]
    pub momentum_12_1: Option<f64>,
    /// Adjusted next-session open to adjusted open after `horizon_sessions`.
    /// Null when the decision-date cross-section member has no entry or exit
    /// bar: membership is settled with decision-date information alone, so a
    /// row that cannot be labelled is still emitted and still carries the
    /// cross-section it belonged to.
    pub target: Option<f64>,
    /// Equal-weight return of the complete eligible decision-date cross-section
    /// over the identical executable holding interval. Rust calculates this
    /// label component before Python sees the matrix.
    #[serde(default)]
    pub market_target: Option<f64>,
    /// Stock-selection target centered within the eligible decision date.
    /// This is a secondary label; raw `target` remains the economic P&L truth.
    #[serde(default)]
    pub relative_target: Option<f64>,
    /// Absolute return divided by decision-time daily volatility. Retained for
    /// compatibility with the original direction-preserving research arm; the
    /// value is finalized in Rust and Python may only fit it.
    #[serde(default)]
    pub return_per_risk_target: Option<f64>,
    /// Relative stock-selection return divided by decision-time daily
    /// volatility, then centered inside the decision-date cross-section.
    /// Inference multiplies the fitted score by the same row's volatility
    /// exactly once before applying return-unit cost gates.
    #[serde(default)]
    pub relative_return_per_risk_target: Option<f64>,
    /// Average cross-sectional rank of the forward return mapped to [-1, 1].
    /// This robust selection label is finalized here so Python never ranks or
    /// otherwise transforms outcomes.
    #[serde(default)]
    pub relative_rank_target: Option<f64>,
    /// Executable adjusted open of the session after the decision date. Null
    /// when that session is missing, which is the only case in which the
    /// position could not have been opened at all.
    pub entry_price: Option<f64>,
    /// Executable adjusted open of the exit session. Null when the history
    /// stops before it.
    pub exit_price: Option<f64>,
    pub adv20_sek: f64,
    pub vol60: f64,
    /// Decision-session annual stock-borrow fee as a decimal rate. Missing
    /// history uses the backtest's conservative fixed fallback.
    #[serde(default)]
    pub borrow_fee_annualized: Option<f64>,
    /// Median of up to 20 causal Nasdaq closing bid/ask spreads, in basis
    /// points, requiring at least ten observed sessions through this decision
    /// date. It is execution-cost context, not a model input; missing history
    /// uses the backtest's disclosed spread fallback.
    #[serde(default)]
    pub median_closing_spread_bps_20: Option<f64>,
    /// Each decision date has total training weight one. The denominator is
    /// every emitted row of the date, labelled or not, because the weight
    /// describes the decision cross-section rather than the trainable subset;
    /// making it depend on label availability would reintroduce the
    /// future-conditioned membership this field is meant to describe.
    pub sample_weight: f64,
    pub features: BTreeMap<String, f64>,
}

impl TrainingRow {
    /// Whether a replay can both open this position and observe what it
    /// returned. Cross-section membership deliberately does not depend on
    /// either fact, so every consumer that turns rows into positions has to
    /// ask, and every one of them must ask the same question.
    ///
    /// A member with no entry bar was never enterable. A member with an entry
    /// bar but no exit bar is a survivorship gap: it stopped trading inside
    /// the holding period and this crate has no terminal value for it, so a
    /// replay cannot honour the position. Once delisted histories carry
    /// terminal values, that second case becomes a labelled row again.
    pub fn is_replayable(&self) -> bool {
        self.entry_price.is_some() && self.target.is_some()
    }

    /// A member that could have been entered but whose outcome is unobserved.
    /// Disclosed rather than silently dropped.
    pub fn entered_without_an_observed_exit(&self) -> bool {
        self.entry_price.is_some() && self.target.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrainingMatrix {
    pub features: Vec<String>,
    pub rows: Vec<TrainingRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureTargetDiagnostic {
    pub feature: String,
    pub decision_dates: usize,
    pub positive_dates: usize,
    pub mean_rank_ic: f64,
}

/// Audit each already-finalized Rust input against the cross-sectional forward
/// return ordering. The calculation is date-local, so common market direction
/// cannot masquerade as stock-selection skill. This routine never changes a
/// feature, label, model, or portfolio decision.
pub fn feature_target_diagnostics(
    rows: &[TrainingRow],
    feature_names: &[String],
) -> Result<Vec<FeatureTargetDiagnostic>, String> {
    if rows.is_empty() {
        return Err("feature diagnostics require matrix rows".into());
    }
    let mut by_date = BTreeMap::<Date, Vec<&TrainingRow>>::new();
    for row in rows {
        // A row whose forward outcome was never observed carries no ordering
        // information; it is skipped rather than treated as a zero return.
        if row.relative_target.or(row.target).is_some() {
            by_date.entry(row.date).or_default().push(row);
        }
    }
    let mut correlations = vec![Vec::<f64>::new(); feature_names.len()];
    for group in by_date.values() {
        let targets = group
            .iter()
            .map(|row| {
                row.relative_target
                    .or(row.target)
                    .expect("unlabelled rows are filtered above")
            })
            .collect::<Vec<_>>();
        for (index, name) in feature_names.iter().enumerate() {
            let values = group
                .iter()
                .map(|row| {
                    row.features.get(name).copied().ok_or_else(|| {
                        format!(
                            "matrix row {} on {} lacks feature {name:?}",
                            row.instrument_id, row.date
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(value) = features_common::spearman(&values, &targets) {
                correlations[index].push(value);
            }
        }
    }
    Ok(feature_names
        .iter()
        .cloned()
        .zip(correlations)
        .map(|(feature, values)| FeatureTargetDiagnostic {
            feature,
            decision_dates: values.len(),
            positive_dates: values.iter().filter(|value| **value > 0.0).count(),
            mean_rank_ic: if values.is_empty() {
                0.0
            } else {
                values.iter().sum::<f64>() / values.len() as f64
            },
        })
        .collect())
}

#[derive(Debug, Clone)]
struct Candidate {
    meta: InstrumentMeta,
    feature: FeatureRow,
    target: Option<f64>,
    entry_price: Option<f64>,
    exit_price: Option<f64>,
    adv20_sek: f64,
    vol60: f64,
    momentum_12_1: f64,
}

/// Construct the final ordered matrix and labels in Rust. Bars after a row's
/// decision date are used only for its declared label, never for its features.
pub fn training_matrix(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    min_adv20_sek: f64,
) -> Result<TrainingMatrix, String> {
    training_matrix_for_feature_set(
        bars,
        instruments,
        start,
        end,
        horizon_sessions,
        min_adv20_sek,
        true,
    )
}

pub fn training_matrix_for_feature_set(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    min_adv20_sek: f64,
    include_context: bool,
) -> Result<TrainingMatrix, String> {
    training_matrix_for_named_feature_set(
        bars,
        instruments,
        start,
        end,
        horizon_sessions,
        min_adv20_sek,
        if include_context {
            FeatureSet::Context
        } else {
            FeatureSet::Baseline
        },
    )
}

pub fn training_matrix_for_named_feature_set(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    min_adv20_sek: f64,
    feature_set: FeatureSet,
) -> Result<TrainingMatrix, String> {
    training_matrix_for_named_feature_set_with_public_shorts(
        bars,
        instruments,
        start,
        end,
        horizon_sessions,
        min_adv20_sek,
        feature_set,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn training_matrix_for_named_feature_set_with_public_shorts(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    min_adv20_sek: f64,
    feature_set: FeatureSet,
    public_short_events: &[PublicShortPositionEvent],
) -> Result<TrainingMatrix, String> {
    training_matrix_for_named_feature_set_with_external_events(
        bars,
        instruments,
        start,
        end,
        horizon_sessions,
        min_adv20_sek,
        feature_set,
        public_short_events,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn training_matrix_for_named_feature_set_with_external_events(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    min_adv20_sek: f64,
    feature_set: FeatureSet,
    public_short_events: &[PublicShortPositionEvent],
    pdmr_events: &[PdmrTransactionEvent],
) -> Result<TrainingMatrix, String> {
    training_matrix_for_named_feature_set_with_all_events(
        bars,
        instruments,
        start,
        end,
        horizon_sessions,
        min_adv20_sek,
        feature_set,
        public_short_events,
        pdmr_events,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn training_matrix_for_named_feature_set_with_all_events(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    min_adv20_sek: f64,
    feature_set: FeatureSet,
    public_short_events: &[PublicShortPositionEvent],
    pdmr_events: &[PdmrTransactionEvent],
    report_events: &[FinancialReportEvent],
) -> Result<TrainingMatrix, String> {
    training_matrix_for_named_feature_set_with_fundamentals(
        bars,
        instruments,
        start,
        end,
        horizon_sessions,
        min_adv20_sek,
        feature_set,
        public_short_events,
        pdmr_events,
        report_events,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn training_matrix_for_named_feature_set_with_fundamentals(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    min_adv20_sek: f64,
    feature_set: FeatureSet,
    public_short_events: &[PublicShortPositionEvent],
    pdmr_events: &[PdmrTransactionEvent],
    report_events: &[FinancialReportEvent],
    fundamental_events: &[AnnualFundamentalEvent],
) -> Result<TrainingMatrix, String> {
    training_matrix_for_named_feature_set_with_all_sources(
        bars,
        instruments,
        start,
        end,
        horizon_sessions,
        min_adv20_sek,
        feature_set,
        public_short_events,
        pdmr_events,
        report_events,
        fundamental_events,
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn training_matrix_for_named_feature_set_with_all_sources(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    min_adv20_sek: f64,
    feature_set: FeatureSet,
    public_short_events: &[PublicShortPositionEvent],
    pdmr_events: &[PdmrTransactionEvent],
    report_events: &[FinancialReportEvent],
    fundamental_events: &[AnnualFundamentalEvent],
    macro_series: &[MacroSeries],
    microstructure: &[MarketMicrostructureBar],
    borrow_fees: &[BorrowFeeBar],
    company_news_events: &[CompanyNewsEvent],
    report_text_events: &[FinancialReportTextEvent],
    global_risk: &[GlobalRiskBar],
    stockholm_close_global_risk: &[GlobalRiskSeries],
) -> Result<TrainingMatrix, String> {
    training_matrix_for_named_feature_set_with_all_sources_and_eligibility(
        bars,
        instruments,
        start,
        end,
        horizon_sessions,
        min_adv20_sek,
        feature_set,
        public_short_events,
        pdmr_events,
        report_events,
        fundamental_events,
        macro_series,
        microstructure,
        borrow_fees,
        company_news_events,
        report_text_events,
        global_risk,
        stockholm_close_global_risk,
        &BTreeMap::new(),
    )
}

/// Construct the final matrix while enforcing known point-in-time eligibility
/// before any cross-sectional feature, relative label, or sample weight is
/// finalized. Missing instruments are deliberately unrestricted: callers must
/// disclose coverage rather than manufacture admission dates.
#[allow(clippy::too_many_arguments)]
pub fn training_matrix_for_named_feature_set_with_all_sources_and_eligibility(
    bars: &[DailyBar],
    instruments: &[InstrumentMeta],
    start: Date,
    end: Date,
    horizon_sessions: usize,
    min_adv20_sek: f64,
    feature_set: FeatureSet,
    public_short_events: &[PublicShortPositionEvent],
    pdmr_events: &[PdmrTransactionEvent],
    report_events: &[FinancialReportEvent],
    fundamental_events: &[AnnualFundamentalEvent],
    macro_series: &[MacroSeries],
    microstructure: &[MarketMicrostructureBar],
    borrow_fees: &[BorrowFeeBar],
    company_news_events: &[CompanyNewsEvent],
    report_text_events: &[FinancialReportTextEvent],
    global_risk: &[GlobalRiskBar],
    stockholm_close_global_risk: &[GlobalRiskSeries],
    eligible_from: &BTreeMap<String, Date>,
) -> Result<TrainingMatrix, String> {
    if end < start {
        return Err("matrix end precedes start".into());
    }
    if horizon_sessions == 0 {
        return Err("label horizon must be positive".into());
    }
    if !min_adv20_sek.is_finite() || min_adv20_sek < 0.0 {
        return Err("minimum ADV must be finite and non-negative".into());
    }
    let meta: BTreeMap<_, _> = instruments
        .iter()
        .filter(|instrument| instrument.bucket.is_stockholm_main_market())
        .cloned()
        .map(|instrument| (instrument.instrument_id.clone(), instrument))
        .collect();
    let features = daily(bars)?;
    let residual = if matches!(
        feature_set,
        FeatureSet::Residual
            | FeatureSet::ResidualPublicShort
            | FeatureSet::DiagnosticsPublicShortLookahead
            | FeatureSet::ResidualPdmr
            | FeatureSet::ResidualPdmrReports
            | FeatureSet::ResidualFundamentals
            | FeatureSet::ResidualQuarterlyFundamentals
            | FeatureSet::ResidualPdmrMacro
            | FeatureSet::ResidualPdmrMicrostructure
            | FeatureSet::ResidualPdmrMicrostructureBorrow
            | FeatureSet::ResidualPdmrMicrostructureBorrowNews
            | FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk
            | FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
            | FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments
    ) {
        residual_features(bars, instruments, eligible_from)?
    } else {
        BTreeMap::new()
    };
    let macro_values = if feature_set == FeatureSet::ResidualPdmrMacro {
        macro_features(bars, instruments, macro_series)?
    } else {
        BTreeMap::new()
    };
    // Execution metadata is independent of the alpha feature contract. When
    // official microstructure is supplied, always carry the causal median
    // spread on TrainingRow so every model family receives the same replay
    // costs; only the selected feature sets expose microstructure to the model.
    let microstructure_rows = if !microstructure.is_empty() {
        microstructure_features(microstructure)?
    } else {
        MicrostructureFeatureRows {
            features: BTreeMap::new(),
            median_spread_bps_20: BTreeMap::new(),
        }
    };
    let microstructure_values = &microstructure_rows.features;
    // Borrow holding cost is likewise a portfolio input, not an alpha feature.
    // Populate it whenever IB fee history is available without adding it to a
    // feature map that did not request the borrow feature family.
    let borrow_fee_values = if !borrow_fees.is_empty() {
        borrow_fee_features(borrow_fees)?
    } else {
        BTreeMap::new()
    };
    let global_risk_values = if feature_set == FeatureSet::BaselineGlobalRisk {
        if global_risk.is_empty() {
            return Err("baseline-global-risk requires global-risk history".into());
        }
        global_risk_features(global_risk)?
    } else {
        BTreeMap::new()
    };
    let stockholm_close_global_risk_values = if feature_set
        == FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk
    {
        if stockholm_close_global_risk.is_empty() {
            return Err(
                "residual-pdmr-microstructure-borrow-news-global-risk requires Stockholm-close CME history"
                    .into(),
            );
        }
        stockholm_close_global_risk_features(stockholm_close_global_risk)?
    } else {
        BTreeMap::new()
    };
    let by_feature: BTreeMap<_, _> = features
        .into_iter()
        .map(|row| ((row.instrument_id.clone(), row.date), row))
        .collect();
    let mut grouped: BTreeMap<&str, Vec<&DailyBar>> = BTreeMap::new();
    for bar in bars {
        grouped.entry(&bar.instrument_id).or_default().push(bar);
    }
    let adv_index = FEATURE_NAMES
        .iter()
        .position(|name| *name == "median_traded_notional_20")
        .expect("catalog contains ADV");
    let vol_index = FEATURE_NAMES
        .iter()
        .position(|name| *name == "vol_60")
        .expect("catalog contains volatility");
    let slow_index = FEATURE_NAMES
        .iter()
        .position(|name| *name == "ret_252")
        .expect("catalog contains slow history gate");
    let mut candidates: BTreeMap<Date, Vec<Candidate>> = BTreeMap::new();
    for (instrument_id, mut history) in grouped {
        let Some(instrument) = meta.get(instrument_id) else {
            continue;
        };
        history.sort_by_key(|bar| bar.date);
        for index in 0..history.len() {
            let decision = history[index].date;
            if decision < start || decision > end {
                continue;
            }
            if eligible_from
                .get(instrument_id)
                .is_some_and(|admission| decision < *admission)
            {
                continue;
            }
            let feature = by_feature
                .get(&(instrument_id.to_owned(), decision))
                .ok_or_else(|| {
                    format!("missing computed feature for {instrument_id} {decision}")
                })?;
            let Some(adv20_sek) = feature.values[adv_index] else {
                continue;
            };
            let Some(vol60) = feature.values[vol_index] else {
                continue;
            };
            let Some(momentum_12_1) = skipped_return(&history, index, 252, 21) else {
                continue;
            };
            if feature.values[slow_index].is_none() || adv20_sek < min_adv20_sek {
                continue;
            }
            // Every gate above reads decision-date information only, so the
            // cross-section is settled here. Whether the stock still trades on
            // the entry or exit session is future information: it decides
            // which labels exist, never who belongs to the group.
            let tradable_open = |offset: usize| {
                index
                    .checked_add(offset)
                    .and_then(|position| history.get(position))
                    .map(|bar| adjusted_open(bar))
                    .filter(|price| price.is_finite() && *price > 0.0)
            };
            let entry_price = tradable_open(1);
            let exit_price = tradable_open(1 + horizon_sessions);
            let target = entry_price
                .zip(exit_price)
                .and_then(|(entry, exit)| finite(exit / entry - 1.0));
            candidates.entry(decision).or_default().push(Candidate {
                meta: instrument.clone(),
                feature: feature.clone(),
                target,
                entry_price,
                exit_price,
                adv20_sek,
                vol60,
                momentum_12_1,
            });
        }
    }
    let names = match feature_set {
        FeatureSet::Baseline => baseline_model_feature_names(),
        FeatureSet::BaselineGlobalRisk => baseline_global_risk_model_feature_names(),
        FeatureSet::Context => model_feature_names(),
        FeatureSet::Residual => residual_model_feature_names(),
        FeatureSet::ResidualPublicShort => public_short_model_feature_names(),
        FeatureSet::DiagnosticsPublicShortLookahead => {
            diagnostics_public_short_lookahead_model_feature_names()
        }
        FeatureSet::ResidualPdmr => pdmr_model_feature_names(),
        FeatureSet::ResidualPdmrReports => pdmr_report_model_feature_names(),
        FeatureSet::ResidualFundamentals => fundamental_model_feature_names(),
        FeatureSet::ResidualQuarterlyFundamentals => quarterly_fundamental_model_feature_names(),
        FeatureSet::ResidualPdmrMacro => pdmr_macro_model_feature_names(),
        FeatureSet::ResidualPdmrMicrostructure => pdmr_microstructure_model_feature_names(),
        FeatureSet::ResidualPdmrMicrostructureBorrow => {
            pdmr_microstructure_borrow_model_feature_names()
        }
        FeatureSet::ResidualPdmrMicrostructureBorrowNews => {
            pdmr_microstructure_borrow_news_model_feature_names()
        }
        FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk => {
            pdmr_microstructure_borrow_news_global_risk_model_feature_names()
        }
        FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
        | FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments => {
            pdmr_microstructure_borrow_news_report_text_model_feature_names()
        }
    };
    // Only the diagnostics-labeled feature set may build the position-date-
    // keyed cursor now; the ResidualPublicShort candidate contract no longer
    // requests these features at all (see public_short_model_feature_names).
    let mut public_short_cursor = (feature_set == FeatureSet::DiagnosticsPublicShortLookahead)
        .then(|| PublicShortCursor::new(public_short_events))
        .transpose()?;
    let mut pdmr_cursor = (feature_set == FeatureSet::ResidualPdmr)
        .then(|| PdmrCursor::new(pdmr_events))
        .transpose()?;
    if feature_set == FeatureSet::ResidualPdmrReports && pdmr_events.is_empty() {
        return Err("residual-pdmr-reports requires PDMR events".into());
    }
    let mut combined_pdmr_cursor = (feature_set == FeatureSet::ResidualPdmrReports)
        .then(|| PdmrCursor::new(pdmr_events))
        .transpose()?;
    let mut report_cursor = (feature_set == FeatureSet::ResidualPdmrReports)
        .then(|| FinancialReportCursor::new(report_events, bars))
        .transpose()?;
    let mut fundamental_cursor = matches!(
        feature_set,
        FeatureSet::ResidualFundamentals | FeatureSet::ResidualQuarterlyFundamentals
    )
    .then(|| AnnualFundamentalCursor::new(fundamental_events, bars))
    .transpose()?;
    let mut macro_pdmr_cursor = (feature_set == FeatureSet::ResidualPdmrMacro)
        .then(|| PdmrCursor::new(pdmr_events))
        .transpose()?;
    let mut microstructure_pdmr_cursor = (feature_set == FeatureSet::ResidualPdmrMicrostructure)
        .then(|| PdmrCursor::new(pdmr_events))
        .transpose()?;
    let mut microstructure_borrow_pdmr_cursor = (feature_set
        == FeatureSet::ResidualPdmrMicrostructureBorrow)
        .then(|| PdmrCursor::new(pdmr_events))
        .transpose()?;
    let news_feature_set = matches!(
        feature_set,
        FeatureSet::ResidualPdmrMicrostructureBorrowNews
            | FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk
    );
    let mut news_pdmr_cursor = news_feature_set
        .then(|| PdmrCursor::new(pdmr_events))
        .transpose()?;
    let mut company_news_cursor = news_feature_set
        .then(|| CompanyNewsCursor::new(company_news_events, bars))
        .transpose()?;
    let report_text_feature_set = matches!(
        feature_set,
        FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
            | FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments
    );
    let mut report_text_pdmr_cursor = report_text_feature_set
        .then(|| PdmrCursor::new(pdmr_events))
        .transpose()?;
    let mut report_text_news_cursor = report_text_feature_set
        .then(|| CompanyNewsCursor::new(company_news_events, bars))
        .transpose()?;
    let mut report_text_cursor = report_text_feature_set
        .then(|| FinancialReportTextCursor::new(report_text_events))
        .transpose()?;
    let mut rows = Vec::new();
    for (date, mut group) in candidates {
        let mut bucket_counts = BTreeMap::new();
        for candidate in &group {
            *bucket_counts
                .entry(candidate.meta.bucket.clone())
                .or_insert(0_usize) += 1;
        }
        group.retain(|candidate| bucket_counts[&candidate.meta.bucket] >= 5);
        if group.is_empty() {
            continue;
        }
        // Labels can only average over the members whose outcome is observed.
        // The group itself is already fixed, so an unobserved outcome removes
        // one term from a label and never a member from the cross-section.
        let observed = group
            .iter()
            .filter_map(|candidate| candidate.target)
            .collect::<Vec<_>>();
        let market_target =
            (!observed.is_empty()).then(|| observed.iter().sum::<f64>() / observed.len() as f64);
        let context = group
            .iter()
            .map(|candidate| CrossSectionInput {
                meta: candidate.meta.clone(),
                feature: candidate.feature.clone(),
            })
            .collect::<Vec<_>>();
        let public_short = if let Some(cursor) = &mut public_short_cursor {
            cursor.advance_before(date);
            group
                .iter()
                .map(|candidate| {
                    (
                        candidate.meta.instrument_id.clone(),
                        cursor.values(&candidate.meta.isin, date),
                    )
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let pdmr = if let Some(cursor) = &mut pdmr_cursor {
            cursor.advance_before(date);
            group
                .iter()
                .map(|candidate| {
                    (
                        candidate.meta.instrument_id.clone(),
                        cursor.values(&candidate.meta.isin, date),
                    )
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let pdmr_reports = if let (Some(pdmr_cursor), Some(report_cursor)) =
            (&mut combined_pdmr_cursor, &mut report_cursor)
        {
            pdmr_cursor.advance_before(date);
            report_cursor.advance_before(date);
            group
                .iter()
                .map(|candidate| {
                    let mut values = pdmr_cursor.values(&candidate.meta.isin, date);
                    values.extend(report_cursor.values(&candidate.meta.instrument_id, date));
                    (candidate.meta.instrument_id.clone(), values)
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let fundamentals = if let Some(cursor) = &mut fundamental_cursor {
            cursor.advance_before(date);
            group
                .iter()
                .map(|candidate| {
                    (
                        candidate.meta.instrument_id.clone(),
                        cursor.values(&candidate.meta.instrument_id, date),
                    )
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let pdmr_macro = if let Some(cursor) = &mut macro_pdmr_cursor {
            cursor.advance_before(date);
            group
                .iter()
                .map(|candidate| {
                    let mut values = cursor.values(&candidate.meta.isin, date);
                    let macro_row = macro_values
                        .get(&(candidate.meta.instrument_id.clone(), date))
                        .cloned()
                        .unwrap_or_else(|| vec![None; MACRO_FEATURE_NAMES.len()]);
                    values.extend(macro_row);
                    (candidate.meta.instrument_id.clone(), values)
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let pdmr_microstructure = if let Some(cursor) = &mut microstructure_pdmr_cursor {
            cursor.advance_before(date);
            group
                .iter()
                .map(|candidate| {
                    let mut values = cursor.values(&candidate.meta.isin, date);
                    let microstructure_row = microstructure_values
                        .get(&(candidate.meta.instrument_id.clone(), date))
                        .cloned()
                        .unwrap_or_else(|| vec![None; MICROSTRUCTURE_FEATURE_NAMES.len()]);
                    values.extend(microstructure_row);
                    (candidate.meta.instrument_id.clone(), values)
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let pdmr_microstructure_borrow =
            if let Some(cursor) = &mut microstructure_borrow_pdmr_cursor {
                cursor.advance_before(date);
                group
                    .iter()
                    .map(|candidate| {
                        let key = (candidate.meta.instrument_id.clone(), date);
                        let mut values = cursor.values(&candidate.meta.isin, date);
                        let microstructure_row = microstructure_values
                            .get(&key)
                            .cloned()
                            .unwrap_or_else(|| vec![None; MICROSTRUCTURE_FEATURE_NAMES.len()]);
                        values.extend(microstructure_row);
                        let borrow_row = borrow_fee_values
                            .get(&key)
                            .cloned()
                            .unwrap_or_else(|| vec![None; BORROW_FEE_FEATURE_NAMES.len()]);
                        values.extend(borrow_row);
                        (candidate.meta.instrument_id.clone(), values)
                    })
                    .collect()
            } else {
                BTreeMap::new()
            };
        let pdmr_microstructure_borrow_news = if let (Some(pdmr_cursor), Some(news_cursor)) =
            (&mut news_pdmr_cursor, &mut company_news_cursor)
        {
            pdmr_cursor.advance_before(date);
            news_cursor.advance_before(date);
            group
                .iter()
                .map(|candidate| {
                    let key = (candidate.meta.instrument_id.clone(), date);
                    let mut values = pdmr_cursor.values(&candidate.meta.isin, date);
                    let microstructure_row = microstructure_values
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| vec![None; MICROSTRUCTURE_FEATURE_NAMES.len()]);
                    values.extend(microstructure_row);
                    let borrow_row = borrow_fee_values
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| vec![None; BORROW_FEE_FEATURE_NAMES.len()]);
                    values.extend(borrow_row);
                    values.extend(news_cursor.values(&candidate.meta.instrument_id, date));
                    (candidate.meta.instrument_id.clone(), values)
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let pdmr_microstructure_borrow_news_report_text =
            if let (Some(pdmr_cursor), Some(news_cursor), Some(report_text_cursor)) = (
                &mut report_text_pdmr_cursor,
                &mut report_text_news_cursor,
                &mut report_text_cursor,
            ) {
                pdmr_cursor.advance_before(date);
                news_cursor.advance_before(date);
                report_text_cursor.advance_before(date);
                group
                    .iter()
                    .map(|candidate| {
                        let key = (candidate.meta.instrument_id.clone(), date);
                        let mut values = pdmr_cursor.values(&candidate.meta.isin, date);
                        let microstructure_row = microstructure_values
                            .get(&key)
                            .cloned()
                            .unwrap_or_else(|| vec![None; MICROSTRUCTURE_FEATURE_NAMES.len()]);
                        values.extend(microstructure_row);
                        let borrow_row = borrow_fee_values
                            .get(&key)
                            .cloned()
                            .unwrap_or_else(|| vec![None; BORROW_FEE_FEATURE_NAMES.len()]);
                        values.extend(borrow_row);
                        values.extend(news_cursor.values(&candidate.meta.instrument_id, date));
                        values
                            .extend(report_text_cursor.values(&candidate.meta.instrument_id, date));
                        (candidate.meta.instrument_id.clone(), values)
                    })
                    .collect()
            } else {
                BTreeMap::new()
            };
        let normalised = match feature_set {
            FeatureSet::Baseline => baseline_cross_section(&context)?,
            FeatureSet::BaselineGlobalRisk => baseline_global_risk_cross_section(
                &context,
                &global_risk_before(&global_risk_values, date),
            )?,
            FeatureSet::Context => model_cross_section(&context)?,
            FeatureSet::Residual => residual_cross_section(&context, &residual)?,
            // Same feature list as Residual now; see
            // public_short_model_feature_names.
            FeatureSet::ResidualPublicShort => residual_cross_section(&context, &residual)?,
            FeatureSet::DiagnosticsPublicShortLookahead => {
                residual_public_short_cross_section(&context, &residual, &public_short)?
            }
            FeatureSet::ResidualPdmr => residual_pdmr_cross_section(&context, &residual, &pdmr)?,
            FeatureSet::ResidualPdmrReports => {
                residual_pdmr_report_cross_section(&context, &residual, &pdmr_reports)?
            }
            FeatureSet::ResidualFundamentals => {
                residual_fundamental_cross_section(&context, &residual, &fundamentals)?
            }
            FeatureSet::ResidualQuarterlyFundamentals => {
                residual_fundamental_cross_section(&context, &residual, &fundamentals)?
            }
            FeatureSet::ResidualPdmrMacro => {
                residual_pdmr_macro_cross_section(&context, &residual, &pdmr_macro)?
            }
            FeatureSet::ResidualPdmrMicrostructure => residual_pdmr_microstructure_cross_section(
                &context,
                &residual,
                &pdmr_microstructure,
            )?,
            FeatureSet::ResidualPdmrMicrostructureBorrow => {
                residual_pdmr_microstructure_borrow_cross_section(
                    &context,
                    &residual,
                    &pdmr_microstructure_borrow,
                )?
            }
            FeatureSet::ResidualPdmrMicrostructureBorrowNews => {
                residual_pdmr_microstructure_borrow_news_cross_section(
                    &context,
                    &residual,
                    &pdmr_microstructure_borrow_news,
                )?
            }
            FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk => {
                append_stockholm_close_global_risk(
                    residual_pdmr_microstructure_borrow_news_cross_section(
                        &context,
                        &residual,
                        &pdmr_microstructure_borrow_news,
                    )?,
                    &stockholm_close_global_risk_values
                        .get(&date)
                        .cloned()
                        .unwrap_or_else(|| {
                            vec![None; stockholm_close_global_risk_feature_names().len()]
                        }),
                )?
            }
            FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
            | FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments => {
                residual_pdmr_microstructure_borrow_news_report_text_cross_section(
                    &context,
                    &residual,
                    &pdmr_microstructure_borrow_news_report_text,
                )?
            }
        };
        let mut ranked_targets = features_common::rank_normalise(
            &observed
                .iter()
                .map(|target| vec![Some(*target)])
                .collect::<Vec<_>>(),
        )?
        .into_iter();
        let relative_risk_mean = market_target.map(|market| {
            group
                .iter()
                .filter_map(|candidate| {
                    candidate
                        .target
                        .map(|target| (target - market) / candidate.vol60)
                })
                .sum::<f64>()
                / observed.len() as f64
        });
        for (candidate, values) in group.into_iter().zip(normalised) {
            let ranked_target = candidate
                .target
                .and_then(|_| ranked_targets.next())
                .and_then(|ranks| ranks.first().copied());
            if values.values.len() != names.len() {
                return Err("Stockholm contextual model row has the wrong width".into());
            }
            let borrow_fee_annualized = borrow_fee_values
                .get(&(candidate.meta.instrument_id.clone(), date))
                .and_then(|values| values.first().copied().flatten());
            let median_closing_spread_bps_20 = microstructure_rows
                .median_spread_bps_20
                .get(&(candidate.meta.instrument_id.clone(), date))
                .copied();
            let relative_target = candidate
                .target
                .zip(market_target)
                .map(|(target, market)| target - market);
            rows.push(TrainingRow {
                date: candidate.feature.date,
                instrument_id: candidate.meta.instrument_id,
                symbol: candidate.meta.symbol,
                isin: candidate.meta.isin,
                sector: candidate.meta.sector,
                bucket: candidate.meta.bucket,
                momentum_12_1: Some(candidate.momentum_12_1),
                target: candidate.target,
                market_target,
                relative_target,
                return_per_risk_target: candidate
                    .target
                    .and_then(|target| finite(target / candidate.vol60)),
                relative_return_per_risk_target: relative_target
                    .zip(relative_risk_mean)
                    .and_then(|(relative, mean)| finite(relative / candidate.vol60 - mean)),
                relative_rank_target: ranked_target,
                entry_price: candidate.entry_price,
                exit_price: candidate.exit_price,
                adv20_sek: candidate.adv20_sek,
                vol60: candidate.vol60,
                borrow_fee_annualized,
                median_closing_spread_bps_20,
                sample_weight: 0.0,
                features: names.iter().cloned().zip(values.values).collect(),
            });
        }
    }
    rows.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then_with(|| a.instrument_id.cmp(&b.instrument_id))
    });
    let mut start_index = 0;
    while start_index < rows.len() {
        let date = rows[start_index].date;
        let mut end_index = start_index + 1;
        while end_index < rows.len() && rows[end_index].date == date {
            end_index += 1;
        }
        let weight = 1.0 / (end_index - start_index) as f64;
        for row in &mut rows[start_index..end_index] {
            row.sample_weight = weight;
        }
        start_index = end_index;
    }
    Ok(TrainingMatrix {
        features: names,
        rows,
    })
}

fn adjusted_open(bar: &DailyBar) -> f64 {
    bar.raw_open * bar.adjusted_close / bar.raw_close
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
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

fn simple_return(history: &[&DailyBar], index: usize, window: usize) -> Option<f64> {
    index.checked_sub(window).and_then(|start| {
        finite(history[index].adjusted_close / history[start].adjusted_close - 1.0)
    })
}

fn skipped_return(
    history: &[&DailyBar],
    index: usize,
    lookback: usize,
    skip: usize,
) -> Option<f64> {
    let start = index.checked_sub(lookback)?;
    let end = index.checked_sub(skip)?;
    finite(history[end].adjusted_close / history[start].adjusted_close - 1.0)
}

fn log_returns(history: &[&DailyBar], index: usize, window: usize) -> Vec<f64> {
    let Some(start) = index.checked_sub(window) else {
        return Vec::new();
    };
    (start + 1..=index)
        .filter_map(|i| finite((history[i].adjusted_close / history[i - 1].adjusted_close).ln()))
        .collect()
}

fn standard_deviation(values: &[f64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    finite(
        (values
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / values.len() as f64)
            .sqrt(),
    )
}

fn downside_deviation(values: &[f64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    finite(
        (values
            .iter()
            .map(|value| value.min(0.0).powi(2))
            .sum::<f64>()
            / values.len() as f64)
            .sqrt(),
    )
}

fn skew(values: &[f64]) -> Option<f64> {
    let deviation = standard_deviation(values)?;
    if deviation <= f64::EPSILON {
        return None;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    finite(
        values
            .iter()
            .map(|value| ((value - mean) / deviation).powi(3))
            .sum::<f64>()
            / values.len() as f64,
    )
}

fn trailing<'a>(
    history: &'a [&DailyBar],
    index: usize,
    window: usize,
) -> Option<&'a [&'a DailyBar]> {
    let start = index.checked_add(1)?.checked_sub(window)?;
    Some(&history[start..=index])
}

fn max_drawdown(history: &[&DailyBar], index: usize, window: usize) -> Option<f64> {
    let bars = trailing(history, index, window)?;
    let mut peak = bars[0].adjusted_close;
    let mut drawdown = 0.0_f64;
    for bar in bars {
        peak = peak.max(bar.adjusted_close);
        drawdown = drawdown.min(bar.adjusted_close / peak - 1.0);
    }
    finite(drawdown)
}

fn distance_from_high(history: &[&DailyBar], index: usize, window: usize) -> Option<f64> {
    let bars = trailing(history, index, window)?;
    let high = bars
        .iter()
        .map(|bar| bar.adjusted_close)
        .fold(f64::NEG_INFINITY, f64::max);
    finite(history[index].adjusted_close / high - 1.0)
}

fn distance_from_low(history: &[&DailyBar], index: usize, window: usize) -> Option<f64> {
    let bars = trailing(history, index, window)?;
    let low = bars
        .iter()
        .map(|bar| bar.adjusted_close)
        .fold(f64::INFINITY, f64::min);
    finite(history[index].adjusted_close / low - 1.0)
}

fn close_location(bar: &DailyBar) -> Option<f64> {
    let width = bar.raw_high - bar.raw_low;
    if width <= f64::EPSILON {
        return Some(0.0);
    }
    finite(2.0 * (bar.raw_close - bar.raw_low) / width - 1.0)
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        finite((values[middle - 1] + values[middle]) / 2.0)
    } else {
        finite(values[middle])
    }
}

fn median_notional(history: &[&DailyBar], index: usize, window: usize) -> Option<f64> {
    let mut notionals = trailing(history, index, window)?
        .iter()
        .map(|bar| bar.raw_close * bar.volume)
        .collect::<Vec<_>>();
    median(&mut notionals)
}

fn amihud(history: &[&DailyBar], index: usize, window: usize) -> Option<f64> {
    let start = index.checked_sub(window)?;
    let mut total = 0.0;
    for i in start + 1..=index {
        let notional = history[i].raw_close * history[i].volume;
        if notional <= 0.0 {
            return None;
        }
        total += (history[i].adjusted_close / history[i - 1].adjusted_close - 1.0).abs() / notional;
    }
    finite(total / window as f64 * 1_000_000.0)
}

fn volume_surge(history: &[&DailyBar], index: usize, window: usize) -> Option<f64> {
    let start = index.checked_sub(window)?;
    // Traded notional (raw_close * volume), not raw share volume: a split
    // changes share counts without changing how much money actually traded,
    // so notional stays continuous while share volume jumps (audit F7). Same
    // construction as `median_notional`/`amihud`.
    let mut prior = history[start..index]
        .iter()
        .map(|bar| bar.raw_close * bar.volume)
        .collect::<Vec<_>>();
    let baseline = median(&mut prior)?;
    if baseline <= 0.0 {
        return None;
    }
    finite((history[index].raw_close * history[index].volume) / baseline)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Duration, Month};

    fn bars(instrument: &str, count: usize, scale: f64) -> Vec<DailyBar> {
        let start = Date::from_calendar_date(2024, Month::January, 1).unwrap();
        let mut previous = 100.0 * scale;
        (0..count)
            .map(|index| {
                let open = previous * (1.0 + (index as f64 * 0.17).sin() * 0.002);
                let close = open * (1.0 + (index as f64 * 0.31).cos() * 0.01);
                previous = close;
                DailyBar {
                    date: start + Duration::days(index as i64),
                    instrument_id: instrument.into(),
                    raw_open: open,
                    raw_high: open.max(close) * 1.005,
                    raw_low: open.min(close) * 0.995,
                    raw_close: close,
                    volume: 10_000.0 + index as f64,
                    adjusted_close: close,
                }
            })
            .collect()
    }

    fn microstructure_bars(instrument: &str, count: usize) -> Vec<MarketMicrostructureBar> {
        let start = Date::from_calendar_date(2024, Month::January, 1).unwrap();
        (0..count)
            .map(|index| {
                let close = 100.0 + index as f64;
                MarketMicrostructureBar {
                    date: start + Duration::days(index as i64),
                    instrument_id: instrument.into(),
                    bid: Some(close - 0.05),
                    ask: Some(close + 0.05),
                    close,
                    average: Some(close - 0.1),
                    turnover_sek: 1_000_000.0 + index as f64 * 10_000.0,
                    trades: Some(1_000 + index as u64),
                }
            })
            .collect()
    }

    #[test]
    fn microstructure_features_are_trailing_and_ignore_future_rows() {
        let full = microstructure_bars("TX1", 30);
        let through_twenty = full[..20].to_vec();
        let date = through_twenty.last().unwrap().date;
        let prefix = microstructure_features(&through_twenty).unwrap();
        let complete = microstructure_features(&full).unwrap();
        let key = &("TX1".into(), date);
        let prefix_row = &prefix.features[key];
        assert_eq!(prefix_row, &complete.features[key]);
        assert_eq!(
            prefix.median_spread_bps_20[key],
            complete.median_spread_bps_20[key]
        );
        assert_eq!(prefix_row.len(), MICROSTRUCTURE_FEATURE_NAMES.len());
        assert!(prefix_row.iter().all(Option::is_some));
        assert!(prefix_row[0].unwrap() > 0.0);
    }

    #[test]
    fn borrow_fee_features_are_trailing_and_keep_decimal_annual_rates() {
        let start = Date::from_calendar_date(2024, Month::January, 1).unwrap();
        let full = (0..30)
            .map(|index| BorrowFeeBar {
                date: start + Duration::days(index),
                instrument_id: "TX1".into(),
                annual_rate: 0.01 + index as f64 / 10_000.0,
            })
            .collect::<Vec<_>>();
        let prefix = borrow_fee_features(&full[..21]).unwrap();
        let complete = borrow_fee_features(&full).unwrap();
        let date = full[20].date;
        let row = &prefix[&("TX1".into(), date)];
        assert_eq!(row, &complete[&("TX1".into(), date)]);
        assert_eq!(row.len(), BORROW_FEE_FEATURE_NAMES.len());
        assert_eq!(row[0], Some(0.012));
        assert!(row.iter().all(Option::is_some));
    }

    #[test]
    fn global_risk_features_are_strictly_prior_day_and_prefix_invariant() {
        let start = Date::from_calendar_date(2023, Month::January, 1).unwrap();
        let full = (0..200)
            .map(|index| GlobalRiskBar {
                date: start + Duration::days(index),
                close: 4_000.0 * 1.001_f64.powi(index as i32),
            })
            .collect::<Vec<_>>();
        let prefix = global_risk_features(&full[..160]).unwrap();
        let complete = global_risk_features(&full).unwrap();
        let decision = full[160].date;

        assert_eq!(
            global_risk_before(&prefix, decision),
            global_risk_before(&complete, decision)
        );
        let prior = global_risk_before(&complete, decision);
        assert!((prior[0].unwrap() - 0.001).abs() < 1e-12);
        assert_eq!(prior.len(), GLOBAL_RISK_FEATURE_NAMES.len());
        assert_eq!(
            baseline_global_risk_model_feature_names().len(),
            baseline_model_feature_names().len() + 2 * GLOBAL_RISK_FEATURE_NAMES.len()
        );
    }

    #[test]
    fn stockholm_close_global_risk_is_exact_date_and_prefix_invariant() {
        let start = Date::from_calendar_date(2023, Month::January, 1).unwrap();
        let series = STOCKHOLM_CLOSE_GLOBAL_RISK_SYMBOLS
            .iter()
            .enumerate()
            .map(|(symbol_index, symbol)| GlobalRiskSeries {
                symbol: (*symbol).into(),
                observations: (0..200)
                    .map(|index| GlobalRiskBar {
                        date: start + Duration::days(index),
                        close: (100.0 + symbol_index as f64 * 25.0) * 1.001_f64.powi(index as i32),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let decision = start + Duration::days(159);
        let full = stockholm_close_global_risk_features(&series).unwrap();
        let prefix = series
            .iter()
            .map(|history| GlobalRiskSeries {
                symbol: history.symbol.clone(),
                observations: history.observations[..160].to_vec(),
            })
            .collect::<Vec<_>>();
        let prefix = stockholm_close_global_risk_features(&prefix).unwrap();

        assert_eq!(prefix[&decision], full[&decision]);
        assert_eq!(
            full[&decision].len(),
            stockholm_close_global_risk_feature_names().len()
        );
        assert!(full[&decision]
            .chunks(GLOBAL_RISK_FEATURE_NAMES.len())
            .all(|values| (values[0].unwrap() - 0.001).abs() < 1e-12));
        assert_eq!(
            pdmr_microstructure_borrow_news_global_risk_model_feature_names().len(),
            pdmr_microstructure_borrow_news_model_feature_names().len()
                + 2 * stockholm_close_global_risk_feature_names().len()
        );
    }

    #[test]
    fn baseline_matrix_carries_execution_metadata_without_exposing_it_as_alpha() {
        let mut all_bars = Vec::new();
        let mut instruments = Vec::new();
        let mut microstructure = Vec::new();
        let mut borrow_fees = Vec::new();
        for index in 0..5 {
            let instrument_id = format!("E{index}");
            let history = bars(&instrument_id, 300, 1.0 + index as f64 / 100.0);
            for (day, bar) in history.iter().enumerate() {
                microstructure.push(MarketMicrostructureBar {
                    date: bar.date,
                    instrument_id: instrument_id.clone(),
                    bid: Some(bar.raw_close - 0.05),
                    ask: Some(bar.raw_close + 0.05),
                    close: bar.raw_close,
                    average: Some(bar.raw_close),
                    turnover_sek: 1_000_000.0,
                    trades: Some(1_000),
                });
                borrow_fees.push(BorrowFeeBar {
                    date: bar.date,
                    instrument_id: instrument_id.clone(),
                    annual_rate: 0.01 + day as f64 / 100_000.0,
                });
            }
            all_bars.extend(history);
            instruments.push(InstrumentMeta {
                instrument_id: instrument_id.clone(),
                symbol: instrument_id,
                isin: format!("SE{index:010}"),
                sector: "Industrials".into(),
                bucket: UniverseBucket::LargeCap,
            });
        }
        let decision = all_bars
            .iter()
            .find(|bar| bar.instrument_id == "E0")
            .unwrap()
            .date
            + Duration::days(280);
        let matrix = training_matrix_for_named_feature_set_with_all_sources(
            &all_bars,
            &instruments,
            decision,
            decision,
            5,
            0.0,
            FeatureSet::Baseline,
            &[],
            &[],
            &[],
            &[],
            &[],
            &microstructure,
            &borrow_fees,
            &[],
            &[],
            &[],
            &[],
        )
        .unwrap();

        assert_eq!(matrix.features, baseline_model_feature_names());
        assert!(!matrix
            .features
            .iter()
            .any(|name| name.starts_with("x_nasdaq_") || name.starts_with("x_ib_")));
        assert_eq!(matrix.rows.len(), 5);
        assert!(matrix
            .rows
            .iter()
            .all(|row| row.median_closing_spread_bps_20.is_some()));
        assert!(matrix
            .rows
            .iter()
            .all(|row| row.borrow_fee_annualized.is_some()));
    }

    #[test]
    fn point_in_time_eligibility_precedes_cross_sectional_labels_and_weights() {
        let mut all_bars = Vec::new();
        let mut instruments = Vec::new();
        for index in 0..6 {
            let instrument_id = format!("M{index}");
            let mut history = bars(&instrument_id, 300, 1.0 + index as f64 / 100.0);
            if index == 5 {
                // Give the instrument that will be ineligible a distinct
                // future outcome. Its removal must change the market label,
                // not merely remove its already-finalized row.
                history[286].adjusted_close *= 2.0;
            }
            all_bars.extend(history);
            instruments.push(InstrumentMeta {
                instrument_id: instrument_id.clone(),
                symbol: instrument_id,
                isin: format!("SE{index:010}"),
                sector: "Industrials".into(),
                bucket: UniverseBucket::LargeCap,
            });
        }
        let decision =
            Date::from_calendar_date(2024, Month::January, 1).unwrap() + Duration::days(280);
        let build = |eligible_from: &BTreeMap<String, Date>| {
            training_matrix_for_named_feature_set_with_all_sources_and_eligibility(
                &all_bars,
                &instruments,
                decision,
                decision,
                5,
                0.0,
                FeatureSet::Baseline,
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                eligible_from,
            )
            .unwrap()
        };

        let unrestricted = build(&BTreeMap::new());
        let restricted = build(&BTreeMap::from([(
            "M5".to_owned(),
            decision + Duration::days(1),
        )]));

        assert_eq!(unrestricted.rows.len(), 6);
        assert_eq!(restricted.rows.len(), 5);
        assert!(restricted.rows.iter().all(|row| row.instrument_id != "M5"));
        assert!(restricted
            .rows
            .iter()
            .all(|row| (row.sample_weight - 0.2).abs() < 1e-12));
        assert_ne!(
            unrestricted.rows[0].market_target, restricted.rows[0].market_target,
            "eligibility must be applied before the market-relative label"
        );
        let diagnostics =
            feature_target_diagnostics(&restricted.rows, &restricted.features).unwrap();
        assert_eq!(diagnostics.len(), restricted.features.len());
        assert!(diagnostics.iter().all(|row| row.decision_dates <= 1));
    }

    const INVARIANCE_HORIZON: usize = 20;
    const INVARIANCE_DECISION_INDEX: usize = 280;
    const INVARIANCE_EXIT_INDEX: usize = INVARIANCE_DECISION_INDEX + 1 + INVARIANCE_HORIZON;

    /// `bars` alone only rescales one shared path, which would leave every
    /// cross-sectional rank tied and the invariance claim vacuous. Give each
    /// line its own phase so features, ranks and forward returns disperse.
    fn dispersed_bars(instrument: &str, phase: usize, count: usize) -> Vec<DailyBar> {
        let mut history = bars(instrument, count, 1.0 + phase as f64 / 100.0);
        for (day, bar) in history.iter_mut().enumerate() {
            let factor = 1.0 + (day as f64 * 0.13 + phase as f64).sin() * 0.05;
            bar.raw_open *= factor;
            bar.raw_high *= factor;
            bar.raw_low *= factor;
            bar.raw_close *= factor;
            bar.adjusted_close *= factor;
        }
        history
    }

    fn invariance_universe(extra_instrument: &str) -> (Vec<Vec<DailyBar>>, Vec<InstrumentMeta>) {
        let mut peers = Vec::new();
        let mut instruments = Vec::new();
        for index in 0..5 {
            let instrument_id = format!("P{index}");
            peers.push(dispersed_bars(
                &instrument_id,
                index,
                INVARIANCE_EXIT_INDEX + 1,
            ));
            instruments.push(InstrumentMeta {
                instrument_id: instrument_id.clone(),
                symbol: instrument_id,
                isin: format!("SE{index:010}"),
                sector: "Industrials".into(),
                bucket: UniverseBucket::LargeCap,
            });
        }
        instruments.push(InstrumentMeta {
            instrument_id: extra_instrument.into(),
            symbol: extra_instrument.into(),
            isin: "SE0000000099".into(),
            sector: "Industrials".into(),
            bucket: UniverseBucket::LargeCap,
        });
        (peers, instruments)
    }

    fn invariance_matrix(all_bars: &[DailyBar], instruments: &[InstrumentMeta]) -> TrainingMatrix {
        let decision = Date::from_calendar_date(2024, Month::January, 1).unwrap()
            + Duration::days(INVARIANCE_DECISION_INDEX as i64);
        training_matrix_for_named_feature_set_with_all_sources_and_eligibility(
            all_bars,
            instruments,
            decision,
            decision,
            INVARIANCE_HORIZON,
            0.0,
            FeatureSet::Baseline,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &BTreeMap::new(),
        )
        .unwrap()
    }

    #[test]
    fn cross_section_membership_ignores_whether_a_peer_survives_the_horizon() {
        let (peers, instruments) = invariance_universe("X");
        // X gets its own ordinary phase; nothing about its forward return is
        // arranged to flatter the market label.
        let truncated_x = dispersed_bars("X", 5, INVARIANCE_DECISION_INDEX + 4);
        let full_x = dispersed_bars("X", 5, INVARIANCE_EXIT_INDEX + 1);

        let build = |x_history: &[DailyBar]| {
            let mut all_bars = peers.concat();
            all_bars.extend_from_slice(x_history);
            invariance_matrix(&all_bars, &instruments)
        };
        let truncated = build(&truncated_x);
        let full = build(&full_x);

        // X stops trading three sessions after the decision date in one arm and
        // survives the whole horizon in the other. Nothing knowable on the
        // decision date may move.
        assert_eq!(truncated.rows.len(), 6);
        assert_eq!(full.rows.len(), 6);
        for (left, right) in truncated.rows.iter().zip(&full.rows) {
            assert_eq!(left.instrument_id, right.instrument_id);
            assert_eq!(
                left.features, right.features,
                "decision-date features, ranks and sector medians must not move"
            );
            assert_eq!(left.sample_weight, right.sample_weight);
            assert!((left.sample_weight - 1.0 / 6.0).abs() < 1e-12);
            assert_eq!(left.momentum_12_1, right.momentum_12_1);
            assert_eq!(left.vol60, right.vol60);
            assert_eq!(left.adv20_sek, right.adv20_sek);
        }
        // A degenerate cross-section ranks every member at 0.0 whatever its
        // size, which would make the identity above vacuous.
        assert!(truncated
            .rows
            .iter()
            .any(|row| row.features["x_ret_21"] != truncated.rows[0].features["x_ret_21"]));
        // Each row's own executable prices and absolute outcome are its own
        // history, so they cannot move either.
        for (left, right) in truncated
            .rows
            .iter()
            .zip(&full.rows)
            .filter(|(row, _)| row.instrument_id != "X")
        {
            assert_eq!(left.target, right.target);
            assert_eq!(left.entry_price, right.entry_price);
            assert_eq!(left.exit_price, right.exit_price);
        }

        // market_target is label space, not decision space: it is the
        // equal-weight outcome of the members whose outcome was observed. When
        // X's outcome is unobservable it drops out of that average, and the
        // peers' market-relative labels follow it. That is not a decision-time
        // leak - no live decision reads a label - and pretending otherwise
        // would mean inventing a return for a stock that stopped trading.
        let mean = |matrix: &TrainingMatrix| {
            let observed = matrix
                .rows
                .iter()
                .filter_map(|row| row.target)
                .collect::<Vec<_>>();
            observed.iter().sum::<f64>() / observed.len() as f64
        };
        for matrix in [&truncated, &full] {
            assert!((matrix.rows[0].market_target.unwrap() - mean(matrix)).abs() < 1e-12);
        }
        assert_eq!(
            truncated.rows.iter().filter_map(|row| row.target).count(),
            5
        );
        assert_eq!(full.rows.iter().filter_map(|row| row.target).count(), 6);
        assert_ne!(
            truncated.rows[0].market_target, full.rows[0].market_target,
            "the market label averages the observed outcomes, so it must move \
             when one member's outcome ceases to exist"
        );
        assert_ne!(
            truncated.rows[0].relative_target, full.rows[0].relative_target,
            "market-relative labels follow their market label"
        );

        let row = truncated
            .rows
            .iter()
            .find(|row| row.instrument_id == "X")
            .unwrap();
        assert_eq!(row.target, None);
        assert_eq!(row.exit_price, None);
        assert!(row.entry_price.is_some());
        assert_eq!(row.relative_target, None);
        assert_eq!(row.return_per_risk_target, None);
        assert_eq!(row.relative_return_per_risk_target, None);
        assert_eq!(row.relative_rank_target, None);
    }

    #[test]
    fn a_stock_without_an_entry_bar_still_shapes_the_decision_cross_section() {
        let (peers, instruments) = invariance_universe("Z");
        let mut all_bars = peers.concat();
        // Z's history stops on the decision date itself, so it can never be
        // entered; its decision-date features are nonetheless knowable.
        all_bars.extend(dispersed_bars("Z", 5, INVARIANCE_DECISION_INDEX + 1));
        let matrix = invariance_matrix(&all_bars, &instruments);

        assert_eq!(matrix.rows.len(), 6);
        assert!(matrix
            .rows
            .iter()
            .all(|row| (row.sample_weight - 1.0 / 6.0).abs() < 1e-12));
        let row = matrix
            .rows
            .iter()
            .find(|row| row.instrument_id == "Z")
            .unwrap();
        assert_eq!(row.entry_price, None);
        assert_eq!(row.exit_price, None);
        assert_eq!(row.target, None);
        assert!(row.market_target.is_some());
        assert!(matrix
            .rows
            .iter()
            .filter(|row| row.instrument_id != "Z")
            .all(|row| row.target.is_some()));
    }

    #[test]
    fn point_in_time_eligibility_also_precedes_residual_factor_returns() {
        let mut all_bars = Vec::new();
        let mut instruments = Vec::new();
        for index in 0..21 {
            let instrument_id = format!("F{index}");
            let mut history = bars(&instrument_id, 320, 1.0 + index as f64 / 100.0);
            if index == 20 {
                // A future admission may have perfectly observable pre-listing
                // prices, but those returns must not enter the Main Market
                // factor used by already-eligible securities.
                for (day, bar) in history.iter_mut().enumerate() {
                    let factor = 1.0 + (day as f64 * 0.31).sin() * 0.20;
                    bar.raw_open *= factor;
                    bar.raw_high *= factor;
                    bar.raw_low *= factor;
                    bar.raw_close *= factor;
                    bar.adjusted_close *= factor;
                }
            }
            all_bars.extend(history);
            instruments.push(InstrumentMeta {
                instrument_id: instrument_id.clone(),
                symbol: instrument_id,
                isin: format!("SE{index:010}"),
                sector: "Industrials".into(),
                bucket: UniverseBucket::LargeCap,
            });
        }
        let decision =
            Date::from_calendar_date(2024, Month::January, 1).unwrap() + Duration::days(280);
        let eligible_from = BTreeMap::from([("F20".to_owned(), decision + Duration::days(1))]);
        let restricted = residual_features(&all_bars, &instruments, &eligible_from).unwrap();
        let reference_bars = all_bars
            .iter()
            .filter(|bar| bar.instrument_id != "F20")
            .cloned()
            .collect::<Vec<_>>();
        let reference =
            residual_features(&reference_bars, &instruments[..20], &BTreeMap::new()).unwrap();
        let unrestricted = residual_features(&all_bars, &instruments, &BTreeMap::new()).unwrap();

        let key = ("F0".to_owned(), decision);
        assert_eq!(restricted[&key].values, reference[&key].values);
        assert_ne!(unrestricted[&key].values, reference[&key].values);
    }

    #[test]
    fn public_short_events_are_strictly_asof_and_threshold_exits_are_not_zero_fills() {
        let event_date = Date::from_calendar_date(2024, Month::January, 10).unwrap();
        let events = vec![
            PublicShortPositionEvent {
                holder: "Fund A".into(),
                isin: "SE0000000001".into(),
                position_date: event_date,
                position_percent: Some(0.7),
            },
            PublicShortPositionEvent {
                holder: "Fund A".into(),
                isin: "SE0000000001".into(),
                position_date: event_date + Duration::days(10),
                position_percent: Some(1.0),
            },
            PublicShortPositionEvent {
                holder: "Fund A".into(),
                isin: "SE0000000001".into(),
                position_date: event_date + Duration::days(45),
                position_percent: None,
            },
        ];
        let mut cursor = PublicShortCursor::new(&events).unwrap();

        cursor.advance_before(event_date);
        assert_eq!(
            cursor.values("SE0000000001", event_date),
            vec![None; PUBLIC_SHORT_FEATURE_NAMES.len()]
        );
        cursor.advance_before(event_date + Duration::days(1));
        assert_eq!(
            cursor.values("SE0000000001", event_date + Duration::days(1))[0],
            Some(0.7)
        );
        assert_eq!(
            cursor.values("SE0000000002", event_date + Duration::days(1)),
            vec![Some(0.0), Some(0.0), Some(0.0), Some(0.0), None, Some(0.0)]
        );
        cursor.advance_before(event_date + Duration::days(10));
        assert_eq!(
            cursor.values("SE0000000001", event_date + Duration::days(10))[0],
            Some(0.7),
            "same-day changes must not enter the decision"
        );
        cursor.advance_before(event_date + Duration::days(46));
        let after_exit = cursor.values("SE0000000001", event_date + Duration::days(46));
        assert_eq!(after_exit[0], Some(0.0));
        assert_eq!(after_exit[1], Some(0.0));
        assert_eq!(after_exit[2], Some(-1.0));
    }

    #[test]
    fn pdmr_features_use_publication_date_and_ignore_non_initial_filings() {
        let published = Date::from_calendar_date(2024, Month::January, 10).unwrap();
        let event = |publication_date, nature: &str, initial_notification, value: f64| {
            PdmrTransactionEvent {
                publication_date,
                transaction_date: published - Duration::days(5),
                pdmr: "Manager A".into(),
                isin: "SE0000000001".into(),
                initial_notification,
                linked_to_share_option_programme: false,
                nature: nature.into(),
                instrument_type: "Share".into(),
                volume: Some(value / 10.0),
                unit: "Quantity".into(),
                price: Some(10.0),
                currency: "SEK".into(),
            }
        };
        let mut foreign = event(published + Duration::days(3), "Acquisition", true, 2_000.0);
        foreign.pdmr = "Manager B".into();
        foreign.currency = "CAD".into();
        let events = vec![
            event(published, "Acquisition", true, 1_000.0),
            event(published + Duration::days(1), "Acquisition", false, 2_000.0),
            foreign,
            event(published + Duration::days(5), "Disposal", true, 500.0),
        ];
        let mut cursor = PdmrCursor::new(&events).unwrap();

        cursor.advance_before(published);
        assert_eq!(
            cursor.values("SE0000000001", published),
            vec![None; PDMR_FEATURE_NAMES.len()],
            "trade date must not make the filing available before publication"
        );
        cursor.advance_before(published + Duration::days(1));
        assert_eq!(
            cursor.values("SE0000000001", published + Duration::days(1))[0],
            Some(1_000.0)
        );
        cursor.advance_before(published + Duration::days(5));
        assert_eq!(
            cursor.values("SE0000000001", published + Duration::days(5))[0],
            Some(1_000.0),
            "same-day filings and amendment rows must not enter the decision"
        );
        cursor.advance_before(published + Duration::days(6));
        let values = cursor.values("SE0000000001", published + Duration::days(6));
        assert_eq!(values[0], Some(500.0));
        assert_eq!(values[1], Some(500.0));
        assert_eq!(values[2], Some(1_000.0));
        assert_eq!(values[3], Some(500.0));
        assert_eq!(values[4], Some(3.0));
        assert_eq!(values[5], Some(2.0));
        assert_eq!(values[6], Some(3.0));
        assert_eq!(
            cursor.values("SE0000000002", published + Duration::days(6)),
            vec![
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                None,
            ]
        );
    }

    #[test]
    fn financial_report_features_are_strictly_asof_and_deduplicate_translations() {
        let history = bars("I1", 50, 1.0);
        let published = history[10].date;
        let event = FinancialReportEvent {
            instrument_id: "I1".into(),
            publication_date: published,
            publication_key: "2024-01-11 18:00:00".into(),
            after_market_close: true,
        };
        let events = vec![event.clone(), event];
        let mut full = FinancialReportCursor::new(&events, &history).unwrap();
        full.advance_before(published);
        assert_eq!(
            full.values("I1", published),
            vec![None; REPORT_EVENT_FEATURE_NAMES.len()]
        );
        let next = history[11].date;
        full.advance_before(next);
        let values = full.values("I1", next);
        assert_eq!(values[0], Some(1.0));
        assert_eq!(values[1], Some(1.0));
        assert_eq!(values[2], Some(1.0));
        assert!(
            (values[3].unwrap() - (history[11].adjusted_close / history[10].adjusted_close - 1.0))
                .abs()
                < 1e-12
        );

        let prefix_history = history[..15].to_vec();
        let mut prefix = FinancialReportCursor::new(&events, &prefix_history).unwrap();
        prefix.advance_before(history[14].date);
        full.advance_before(history[14].date);
        assert_eq!(
            prefix.values("I1", history[14].date),
            full.values("I1", history[14].date),
            "future prices must not change an earlier report reaction"
        );
    }

    #[test]
    fn company_news_features_are_strictly_asof_and_prefix_invariant() {
        let history = bars("I1", 50, 1.0);
        let published = history[10].date;
        let event = CompanyNewsEvent {
            instrument_id: "I1".into(),
            publication_date: published,
            publication_key: "123".into(),
            after_market_close: true,
            kind: CompanyNewsKind::InsideInformation,
        };
        let events = vec![event.clone(), event];
        let mut full = CompanyNewsCursor::new(&events, &history).unwrap();
        full.advance_before(published);
        assert_eq!(
            full.values("I1", published),
            vec![None; COMPANY_NEWS_FEATURE_NAMES.len()]
        );
        let next = history[11].date;
        full.advance_before(next);
        let values = full.values("I1", next);
        assert_eq!(values.len(), COMPANY_NEWS_FEATURE_NAMES.len());
        assert_eq!(values[0], Some(1.0));
        assert_eq!(values[1], Some(1.0));
        assert_eq!(values[2], Some(1.0));
        assert_eq!(values[3], Some(1.0));
        assert_eq!(values[4], Some(1.0));
        assert!(
            (values[5].unwrap() - (history[11].adjusted_close / history[10].adjusted_close - 1.0))
                .abs()
                < 1e-12
        );
        assert_eq!(values[8..], [Some(0.0); 5]);

        let prefix_history = history[..15].to_vec();
        let mut prefix = CompanyNewsCursor::new(&events, &prefix_history).unwrap();
        prefix.advance_before(history[14].date);
        full.advance_before(history[14].date);
        assert_eq!(
            prefix.values("I1", history[14].date),
            full.values("I1", history[14].date),
            "future prices must not change an earlier company-news reaction"
        );
    }

    #[test]
    fn report_text_metrics_are_bilingual_rust_owned_and_strictly_asof() {
        let published = Date::from_calendar_date(2024, Month::January, 10).unwrap();
        let body = concat!(
            "Net sales for the quarter amounted to SEK 414 (405) million. ",
            "EBIT amounted to SEK 66 (63) million. ",
            "EBIT margin amounted to 15.9 (15.5) percent. ",
            "Earnings per share amounted to SEK 0.49 (0.46)."
        );
        let event = FinancialReportTextEvent {
            instrument_id: "I1".into(),
            publication_date: published,
            publication_key: "2024-01-10 07:00:00".into(),
            language: "en".into(),
            body_text: body.into(),
            extracted_metrics: None,
        };
        let mut cursor = FinancialReportTextCursor::new(&[event.clone(), event]).unwrap();
        cursor.advance_before(published);
        assert_eq!(
            cursor.values("I1", published),
            vec![None; REPORT_TEXT_FEATURE_NAMES.len()]
        );
        let decision = published + Duration::days(1);
        cursor.advance_before(decision);
        let values = cursor.values("I1", decision);
        assert_eq!(values.len(), REPORT_TEXT_FEATURE_NAMES.len());
        assert_eq!(values[0], Some(1.0));
        assert!((values[1].unwrap() - (414.0 / 405.0 - 1.0)).abs() < 1e-12);
        assert_eq!(values[2], None);
        assert!((values[3].unwrap() - (66.0 / 63.0 - 1.0)).abs() < 1e-12);
        assert!((values[4].unwrap() - 0.159).abs() < 1e-12);
        assert!((values[5].unwrap() - 0.004).abs() < 1e-12);
        assert!((values[6].unwrap() - (0.49 / 0.46 - 1.0)).abs() < 1e-12);

        let swedish = extract_report_text_metrics(
            "Nettoomsättning 2 018 (1 925) MSEK. Rörelseresultat 422 (357) MSEK. Rörelsemarginal 20,9 (18,6) procent.",
        );
        assert!((swedish[0].unwrap() - (2018.0 / 1925.0 - 1.0)).abs() < 1e-12);
        assert!((swedish[2].unwrap() - (422.0 / 357.0 - 1.0)).abs() < 1e-12);
        assert!((swedish[3].unwrap() - 0.209).abs() < 1e-12);
    }

    #[test]
    fn report_attachment_metrics_fill_missing_body_values_without_overwriting_them() {
        let body = "Net sales amounted to SEK 110 (100) million.";
        let supplement = concat!(
            "Net sales amounted to SEK 999 (100) million. ",
            "EBIT amounted to SEK 22 (20) million."
        );
        let metrics = report_text_metrics_with_supplements(body, [supplement]);
        let sales = REPORT_TEXT_FEATURE_NAMES[1..]
            .iter()
            .position(|name| *name == "nasdaq_report_sales_growth")
            .unwrap();
        let ebit = REPORT_TEXT_FEATURE_NAMES[1..]
            .iter()
            .position(|name| *name == "nasdaq_report_ebit_growth")
            .unwrap();
        assert!((metrics[sales].unwrap() - 0.1).abs() < 1e-12);
        assert!((metrics[ebit].unwrap() - 0.1).abs() < 1e-12);
    }

    #[test]
    fn annual_fundamentals_are_strictly_asof_and_ratios_are_rust_owned() {
        let history = bars("I1", 50, 1.0);
        let available = history[10].date;
        let event = AnnualFundamentalEvent {
            instrument_id: "I1".into(),
            available_date: available,
            report_period_end: available - Duration::days(10),
            filing_key: "filing-1".into(),
            reporting_currency: Some("iso4217:SEK".into()),
            revenue: Some(400.0),
            prior_revenue: Some(320.0),
            operating_profit: Some(60.0),
            net_income: Some(50.0),
            prior_net_income: Some(30.0),
            assets: Some(1_000.0),
            prior_assets: Some(900.0),
            equity: Some(500.0),
            prior_equity: Some(450.0),
            cash: Some(100.0),
            operating_cash_flow: Some(80.0),
            current_assets: Some(300.0),
            current_liabilities: Some(150.0),
            basic_eps: Some(5.0),
            weighted_average_shares: Some(10.0),
        };
        let events = [event];
        let mut cursor = AnnualFundamentalCursor::new(&events, &history).unwrap();
        cursor.advance_before(available);
        assert_eq!(
            cursor.values("I1", available),
            vec![None; FUNDAMENTAL_FEATURE_NAMES.len()]
        );
        let decision = history[11].date;
        cursor.advance_before(decision);
        let values = cursor.values("I1", decision);
        assert_eq!(values[0], Some(1.0));
        assert_eq!(values[1], Some(0.5));
        assert_eq!(values[2], Some(0.1));
        assert_eq!(values[3], Some(0.08));
        assert_eq!(values[4], Some(-0.03));
        assert_eq!(values[7], Some(0.25));
        assert_eq!(values[11], Some(2.0));
        assert_eq!(
            cursor.values("I2", decision),
            vec![None; FUNDAMENTAL_FEATURE_NAMES.len()]
        );
    }

    #[test]
    fn later_restatement_of_old_period_does_not_replace_newer_quarter() {
        let history = bars("I1", 50, 1.0);
        let template = AnnualFundamentalEvent {
            instrument_id: "I1".into(),
            available_date: history[10].date,
            report_period_end: history[5].date,
            filing_key: "old-original".into(),
            reporting_currency: Some("iso4217:SEK".into()),
            revenue: Some(100.0),
            prior_revenue: Some(90.0),
            operating_profit: Some(10.0),
            net_income: Some(8.0),
            prior_net_income: Some(7.0),
            assets: Some(200.0),
            prior_assets: Some(190.0),
            equity: Some(80.0),
            prior_equity: Some(70.0),
            cash: Some(20.0),
            operating_cash_flow: Some(9.0),
            current_assets: Some(50.0),
            current_liabilities: Some(25.0),
            basic_eps: Some(0.8),
            weighted_average_shares: Some(10.0),
        };
        let mut newer = template.clone();
        newer.available_date = history[20].date;
        newer.report_period_end = history[15].date;
        newer.filing_key = "newer-quarter".into();
        newer.equity = Some(120.0);
        let mut old_restatement = template;
        old_restatement.available_date = history[21].date;
        old_restatement.filing_key = "old-restatement".into();
        old_restatement.equity = Some(20.0);
        let events = [newer, old_restatement];
        let mut cursor = AnnualFundamentalCursor::new(&events, &history).unwrap();
        let decision = history[22].date;
        cursor.advance_before(decision);
        let values = cursor.values("I1", decision);
        assert_eq!(values[0], Some(2.0));
        assert_eq!(values[1], Some(0.6));
    }

    #[test]
    fn macro_exposure_features_are_causal_and_require_declared_series() {
        let history = bars("I1", 300, 1.0);
        let instrument = InstrumentMeta {
            instrument_id: "I1".into(),
            symbol: "I1".into(),
            isin: "SE0000000001".into(),
            sector: "Industrials".into(),
            bucket: UniverseBucket::LargeCap,
        };
        let series = REQUIRED_MACRO_SERIES
            .iter()
            .enumerate()
            .map(|(series_index, series_id)| MacroSeries {
                series_id: (*series_id).into(),
                observations: history
                    .iter()
                    .enumerate()
                    .map(|(index, bar)| MacroObservation {
                        date: bar.date,
                        value: 10.0
                            * (1.0
                                + (index as f64 * (0.07 + series_index as f64 * 0.01)).sin()
                                    * 0.02),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let full = macro_features(&history, std::slice::from_ref(&instrument), &series).unwrap();
        let decision = history[220].date;
        let full_row = full.get(&("I1".into(), decision)).unwrap();
        assert_eq!(full_row.len(), MACRO_FEATURE_NAMES.len());
        assert!(full_row.iter().all(Option::is_some));

        let prefix =
            macro_features(&history[..=220], std::slice::from_ref(&instrument), &series).unwrap();
        assert_eq!(
            prefix.get(&("I1".into(), decision)),
            Some(full_row),
            "future stock bars must not alter earlier FX exposure"
        );
        assert!(macro_features(&history, &[instrument], &series[..2]).is_err());
    }

    fn market_bars(count: usize, daily_return: f64) -> Vec<MarketBar> {
        let start = Date::from_calendar_date(2020, Month::January, 1).unwrap();
        (0..count)
            .scan(100.0, |close, index| {
                if index > 0 {
                    *close *= 1.0 + daily_return;
                }
                Some(MarketBar {
                    date: start + Duration::days(index as i64),
                    close: *close,
                })
            })
            .collect()
    }

    fn market_index_series(symbol: &str, count: usize, daily_return: f64) -> MarketIndexSeries {
        let start = Date::from_calendar_date(2020, Month::January, 1).unwrap();
        let bars = (0..count)
            .scan(100.0, |value, index| {
                if index > 0 {
                    *value *= 1.0 + daily_return;
                }
                Some(MarketIndexBar {
                    date: start + Duration::days(index as i64),
                    start_value: *value,
                    end_value: *value * (1.0 + daily_return / 3.0),
                })
            })
            .collect();
        MarketIndexSeries {
            symbol: symbol.into(),
            bars,
        }
    }

    fn direction_indexes(count: usize, sector_start: usize) -> Vec<MarketIndexSeries> {
        DIRECTION_INDEX_SYMBOLS
            .iter()
            .enumerate()
            .map(|(index, symbol)| {
                let mut series =
                    market_index_series(symbol, count, 0.0005 + index as f64 * 0.000_01);
                if DIRECTION_SECTOR_SYMBOLS.contains(symbol) {
                    series.bars.drain(..sector_start.min(series.bars.len()));
                }
                series
            })
            .collect()
    }

    #[test]
    fn market_trend_is_causal_and_prefix_invariant() {
        let bars = market_bars(320, 0.001);
        let full = market_trend(&bars).unwrap();
        let prefix = market_trend(&bars[..300]).unwrap();
        assert_eq!(prefix, full[..prefix.len()]);
        assert_eq!(full.first().unwrap().date, bars[252].date);
        assert!(full.iter().all(|observation| observation.score == 1.0));
    }

    #[test]
    fn market_trend_detects_a_broad_decline_and_rejects_bad_ordering() {
        let mut bars = market_bars(300, -0.001);
        let observations = market_trend(&bars).unwrap();
        assert!(observations
            .iter()
            .all(|observation| observation.score == -1.0));
        bars.swap(10, 11);
        assert!(market_trend(&bars).is_err());
    }

    /// Every index in the direction set, flat except for a single 1% move that
    /// happens entirely in the gap into `gap_session`. `start_value` is the
    /// prior session's close throughout — the OMXSGI archive's actual
    /// convention — so the archive's SOD cannot see that gap at all.
    fn overnight_gap_direction_indexes(count: usize, gap_session: usize) -> Vec<MarketIndexSeries> {
        let start = Date::from_calendar_date(2020, Month::January, 1).unwrap();
        DIRECTION_INDEX_SYMBOLS
            .iter()
            .map(|symbol| {
                let mut close = 100.0;
                let bars = (0..count)
                    .map(|index| {
                        let previous_close = close;
                        if index == gap_session {
                            close *= 1.01;
                        }
                        MarketIndexBar {
                            date: start + Duration::days(index as i64),
                            start_value: previous_close,
                            end_value: close,
                        }
                    })
                    .collect();
                MarketIndexSeries {
                    symbol: (*symbol).into(),
                    bars,
                }
            })
            .collect()
    }

    #[test]
    fn direction_label_starts_at_the_first_tradable_close_not_the_prior_close() {
        // The decision is taken at the close of session 252; the whole 1% move
        // into session 253 is an opening gap nobody holding cash overnight can
        // capture. A label anchored at `start_value` on session 253 is anchored
        // at the close of session 252 and collects that gap; the label must
        // start at session 253's own close instead.
        let indexes = overnight_gap_direction_indexes(480, 253);
        let primary = indexes
            .iter()
            .find(|series| series.symbol == "OMXSGI")
            .unwrap();
        let decision = primary.bars[252].date;
        let matrix = direction_training_matrix(&indexes, decision, decision, 20).unwrap();
        let row = &matrix.rows[0];
        assert_eq!(row.date, decision);
        assert_eq!(row.entry_value, primary.bars[253].end_value);
        assert_eq!(row.exit_value, primary.bars[273].end_value);
        assert!(
            row.target.abs() < 1e-12,
            "the only move in the window is the untradable gap into the first held session, so the label must be flat, not {}",
            row.target
        );
        let prior_close_anchored =
            primary.bars[273].start_value / primary.bars[253].start_value - 1.0;
        assert!(
            (prior_close_anchored - 0.01).abs() < 1e-12,
            "the retired SOD-anchored label credited the full 1% gap, got {prior_close_anchored}"
        );
    }

    #[test]
    fn direction_matrix_is_causal_and_uses_a_forward_close_label() {
        let indexes = direction_indexes(480, 300);
        let primary = indexes
            .iter()
            .find(|series| series.symbol == "OMXSGI")
            .unwrap();
        let full =
            direction_training_matrix(&indexes, primary.bars[252].date, primary.bars[470].date, 20)
                .unwrap();
        assert_eq!(full.features, direction_model_feature_names());
        assert_eq!(full.rows[0].date, primary.bars[252].date);
        assert_eq!(full.rows[0].entry_value, primary.bars[253].end_value);
        assert_eq!(full.rows[0].exit_value, primary.bars[273].end_value);
        assert!(
            (full.rows[0].target
                - (primary.bars[273].end_value / primary.bars[253].end_value - 1.0))
                .abs()
                < 1e-12
        );
        assert_eq!(full.rows[0].sign_target, full.rows[0].target.signum());
        assert_eq!(
            full.rows[0].features["m_sector_breadth_positive_21"], 1.0,
            "early sector history must be marked missing rather than backfilled"
        );
        assert_eq!(
            full.rows.last().unwrap().features["m_sector_breadth_positive_126"],
            0.0
        );

        let cutoff = primary.bars[420].date;
        let prefix_indexes = indexes
            .iter()
            .cloned()
            .map(|mut series| {
                series.bars.retain(|bar| bar.date <= cutoff);
                series
            })
            .collect::<Vec<_>>();
        let prefix = direction_training_matrix(
            &prefix_indexes,
            primary.bars[252].date,
            primary.bars[470].date,
            20,
        )
        .unwrap();
        assert_eq!(prefix.rows, full.rows[..prefix.rows.len()]);
    }

    #[test]
    fn direction_global_risk_matrix_uses_only_prior_utc_days() {
        let indexes = direction_indexes(480, 300);
        let primary = indexes
            .iter()
            .find(|series| series.symbol == "OMXSGI")
            .unwrap();
        let start = primary.bars[252].date;
        let end = primary.bars[470].date;
        let global = primary
            .bars
            .iter()
            .enumerate()
            .map(|(index, bar)| GlobalRiskBar {
                date: bar.date,
                close: 4_000.0 * 1.001_f64.powi(index as i32),
            })
            .collect::<Vec<_>>();

        let full =
            direction_training_matrix_with_global_risk(&indexes, start, end, 20, &global).unwrap();
        assert_eq!(full.features, direction_global_risk_model_feature_names());
        assert_eq!(
            full.features.len(),
            direction_model_feature_names().len() + 2 * GLOBAL_RISK_FEATURE_NAMES.len()
        );
        assert!((full.rows[0].features["g_es_ret_1"] - 0.001).abs() < 1e-12);
        assert_eq!(full.rows[0].features["g_missing_es_ret_1"], 0.0);

        let same_day = global.iter().position(|bar| bar.date == start).unwrap();
        let prefix = direction_training_matrix_with_global_risk(
            &indexes,
            start,
            end,
            20,
            &global[..=same_day],
        )
        .unwrap();
        assert_eq!(
            prefix.rows[0], full.rows[0],
            "same-day and future CME bars must not alter the decision row"
        );
    }

    #[test]
    fn direction_stockholm_close_global_risk_uses_exact_date_causally() {
        let indexes = direction_indexes(480, 300);
        let primary = indexes
            .iter()
            .find(|series| series.symbol == "OMXSGI")
            .unwrap();
        let start = primary.bars[252].date;
        let end = primary.bars[470].date;
        let global = STOCKHOLM_CLOSE_GLOBAL_RISK_SYMBOLS
            .iter()
            .enumerate()
            .map(|(symbol_index, symbol)| GlobalRiskSeries {
                symbol: (*symbol).into(),
                observations: primary
                    .bars
                    .iter()
                    .enumerate()
                    .map(|(index, bar)| GlobalRiskBar {
                        date: bar.date,
                        close: (4_000.0 + symbol_index as f64 * 500.0)
                            * 1.001_f64.powi(index as i32),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();

        let full = direction_training_matrix_with_stockholm_close_global_risk(
            &indexes, start, end, 20, &global,
        )
        .unwrap();
        assert_eq!(
            full.features,
            direction_stockholm_close_global_risk_model_feature_names()
        );
        assert!((full.rows[0].features["g_es_ret_1"] - 0.001).abs() < 1e-12);
        assert!((full.rows[0].features["g_nq_ret_1"] - 0.001).abs() < 1e-12);

        let first_row = global
            .iter()
            .map(|history| GlobalRiskSeries {
                symbol: history.symbol.clone(),
                observations: history.observations[..=252].to_vec(),
            })
            .collect::<Vec<_>>();
        let prefix = direction_training_matrix_with_stockholm_close_global_risk(
            &indexes, start, end, 20, &first_row,
        )
        .unwrap();
        assert_eq!(
            prefix.rows[0], full.rows[0],
            "later futures bars must not alter the close-time decision row"
        );
    }

    #[test]
    fn direction_matrix_requires_the_declared_official_indexes() {
        let mut indexes = direction_indexes(300, 0);
        indexes.retain(|series| series.symbol != "OMXSBGI");
        let start = indexes[0].bars[252].date;
        let end = indexes[0].bars[270].date;
        assert!(direction_training_matrix(&indexes, start, end, 5)
            .unwrap_err()
            .contains("OMXSBGI"));
    }

    #[test]
    fn features_are_prefix_invariant() {
        let input = bars("SE0000115446", 300, 1.0);
        let full = daily(&input).unwrap();
        let prefix = daily(&input[..260]).unwrap();
        assert_eq!(prefix, full[..260]);
    }

    #[test]
    fn adjusted_returns_do_not_use_raw_discontinuities() {
        let mut input = bars("SE0000115446", 30, 1.0);
        input[29].raw_close *= 0.5;
        input[29].raw_open *= 0.5;
        input[29].raw_high *= 0.5;
        input[29].raw_low *= 0.5;
        let rows = daily(&input).unwrap();
        let last = rows.last().unwrap();
        let expected = input[29].adjusted_close / input[28].adjusted_close - 1.0;
        assert!((last.value("ret_1").unwrap() - expected).abs() < 1e-12);
    }

    /// A 2:1 split lands between t-1 and t: share counts double, raw prices
    /// halve, volume doubles, and the true (adjusted) price and traded
    /// notional are unchanged. `gap_1` and `volume_surge_20` must not mistake
    /// the corporate action for a real overnight move or a real liquidity
    /// surge (audit F6/F7).
    fn flat_bars_with_split_on_last_day(count: usize) -> Vec<DailyBar> {
        let start = Date::from_calendar_date(2024, Month::January, 1).unwrap();
        let mut input: Vec<DailyBar> = (0..count)
            .map(|index| DailyBar {
                date: start + Duration::days(index as i64),
                instrument_id: "SE0000999999".into(),
                raw_open: 100.0,
                raw_high: 100.5,
                raw_low: 99.5,
                raw_close: 100.0,
                volume: 10_000.0,
                adjusted_close: 100.0,
            })
            .collect();
        let last = input.len() - 1;
        input[last].raw_open *= 0.5;
        input[last].raw_high *= 0.5;
        input[last].raw_low *= 0.5;
        input[last].raw_close *= 0.5;
        input[last].volume *= 2.0;
        // adjusted_close is left at the pre-split level: the true price did
        // not move, so it must stay continuous across the split.
        input
    }

    #[test]
    fn gap_1_survives_a_split_with_unchanged_true_price() {
        let input = flat_bars_with_split_on_last_day(25);
        let rows = daily(&input).unwrap();
        let row = rows.last().unwrap();
        let gap = row.value("gap_1").unwrap();
        assert!(
            gap.abs() < 1e-9,
            "gap_1 should be ~0 across a split with unchanged true price, got {gap}"
        );
    }

    #[test]
    fn volume_surge_20_survives_a_split_with_flat_true_traded_notional() {
        let input = flat_bars_with_split_on_last_day(25);
        let rows = daily(&input).unwrap();
        let row = rows.last().unwrap();
        let surge = row.value("volume_surge_20").unwrap();
        assert!(
            (surge - 1.0).abs() < 1e-9,
            "volume_surge_20 should be ~1 when true traded notional is flat, got {surge}"
        );
    }

    #[test]
    fn cross_section_is_rust_ranked() {
        let mut a = daily(&bars("A", 30, 1.0)).unwrap().pop().unwrap();
        let mut b = a.clone();
        b.instrument_id = "B".into();
        let ret = FEATURE_NAMES
            .iter()
            .position(|name| *name == "ret_21")
            .unwrap();
        a.values[ret] = Some(-0.1);
        b.values[ret] = Some(0.2);
        let out = normalise_cross_section(&[a, b]).unwrap();
        assert_eq!(out[0].values[ret], -1.0);
        assert_eq!(out[1].values[ret], 1.0);
    }

    #[test]
    fn feature_selection_is_versioned_and_closed() {
        validate_selection(&["x_ret_21".into()]).unwrap();
        validate_selection(&["x_sector_rel_ret_21".into()]).unwrap();
        validate_selection(&["x_market_breadth_positive_21".into()]).unwrap();
        validate_selection(&["x_market_resid_ret_126".into()]).unwrap();
        validate_selection(&["g_es_ret_21".into()]).unwrap();
        assert!(validate_selection(&["ret_21".into()]).is_err());
        assert!(validate_selection(&["x_made_up".into()]).is_err());
    }

    #[test]
    fn every_stock_feature_set_version_is_distinct_and_past_the_pre_membership_fix_block() {
        let versions = [
            FEATURE_SET_VERSION,
            BASELINE_FEATURE_SET_VERSION,
            BASELINE_GLOBAL_RISK_FEATURE_SET_VERSION,
            RESIDUAL_FEATURE_SET_VERSION,
            PUBLIC_SHORT_FEATURE_SET_VERSION,
            PDMR_FEATURE_SET_VERSION,
            REPORT_EVENT_FEATURE_SET_VERSION,
            FUNDAMENTAL_FEATURE_SET_VERSION,
            QUARTERLY_FUNDAMENTAL_FEATURE_SET_VERSION,
            PDMR_MACRO_FEATURE_SET_VERSION,
            PDMR_MICROSTRUCTURE_FEATURE_SET_VERSION,
            PDMR_MICROSTRUCTURE_BORROW_FEATURE_SET_VERSION,
            PDMR_MICROSTRUCTURE_BORROW_NEWS_FEATURE_SET_VERSION,
            PDMR_MICROSTRUCTURE_BORROW_NEWS_GLOBAL_RISK_FEATURE_SET_VERSION,
            PDMR_MICROSTRUCTURE_BORROW_NEWS_REPORT_TEXT_FEATURE_SET_VERSION,
            PDMR_MICROSTRUCTURE_BORROW_NEWS_REPORT_ATTACHMENTS_FEATURE_SET_VERSION,
        ];
        assert_eq!(
            versions.iter().collect::<BTreeSet<_>>().len(),
            versions.len()
        );
        // Every set shares the candidate-building path, so none may still
        // carry a number issued before decision-date membership was fixed.
        for version in versions {
            let ordinal: usize = version
                .strip_prefix("fs-rust-stockholm-")
                .expect("stock feature-set versions share one prefix")
                .parse()
                .expect("stock feature-set versions end in an ordinal");
            assert!(ordinal > 16, "{version} predates the membership fix");
        }
    }

    #[test]
    fn residual_public_short_no_longer_carries_the_lookahead_prone_short_family() {
        // Task 7(d): FI's historical short-register events are keyed by
        // position_date, and late filings backfill that date -- a real
        // look-ahead risk for any candidate model trained on it. The
        // ResidualPublicShort candidate contract (fs-rust-stockholm-20) is
        // amended in place (no version bump: Task 5 just renumbered every
        // set and no real matrix has been built under the new numbers yet)
        // to drop the family entirely. The historical cursor survives only
        // behind the explicitly diagnostics-labeled FeatureSet.
        let names = public_short_model_feature_names();
        for short_name in PUBLIC_SHORT_FEATURE_NAMES {
            assert!(
                !names.iter().any(|name| name.ends_with(short_name)),
                "candidate public-short contract still carries {short_name}"
            );
        }
        assert_eq!(names, residual_model_feature_names());

        // The diagnostics-only path still carries the full family.
        let diagnostics_names = diagnostics_public_short_lookahead_model_feature_names();
        for short_name in PUBLIC_SHORT_FEATURE_NAMES {
            assert!(
                diagnostics_names
                    .iter()
                    .any(|name| name.ends_with(short_name)),
                "diagnostics contract lost {short_name}"
            );
        }

        // A candidate matrix under the amended contract builds with no
        // public-short events at all -- it no longer needs (or accepts) them.
        let mut all_bars = Vec::new();
        let mut instruments = Vec::new();
        for index in 0..20 {
            let id = format!("P{index}");
            all_bars.extend(bars(&id, 330, 1.0 + index as f64 / 100.0));
            instruments.push(InstrumentMeta {
                instrument_id: id,
                symbol: format!("PS{index}"),
                isin: format!("SE{index:010}"),
                sector: if index < 10 { "A" } else { "B" }.into(),
                bucket: UniverseBucket::LargeCap,
            });
        }
        let cutoff =
            Date::from_calendar_date(2024, Month::January, 1).unwrap() + Duration::days(299);
        let matrix = training_matrix_for_named_feature_set(
            &all_bars,
            &instruments,
            cutoff,
            cutoff + Duration::days(20),
            5,
            0.0,
            FeatureSet::ResidualPublicShort,
        )
        .unwrap();
        assert_eq!(matrix.features, residual_model_feature_names());
    }

    #[test]
    fn residual_features_are_causal_and_finalized_in_rust() {
        let mut all_bars = Vec::new();
        let mut instruments = Vec::new();
        for index in 0..20 {
            let id = format!("R{index}");
            let mut history = bars(&id, 330, 1.0 + index as f64 / 100.0);
            // Give the two sectors distinct but causal return paths so the
            // market/sector factor regression is identified.
            for (day, bar) in history.iter_mut().enumerate() {
                let tilt = 1.0
                    + ((day as f64 * 0.07 + index as f64 * 0.13).sin() * (index as f64 - 9.5)
                        / 10_000.0);
                bar.raw_open *= tilt;
                bar.raw_high *= tilt;
                bar.raw_low *= tilt;
                bar.raw_close *= tilt;
                bar.adjusted_close *= tilt;
            }
            all_bars.extend(history);
            instruments.push(InstrumentMeta {
                instrument_id: id,
                symbol: format!("RS{index}"),
                isin: format!("SE{index:010}"),
                sector: if index < 10 { "A" } else { "B" }.into(),
                bucket: UniverseBucket::LargeCap,
            });
        }
        let cutoff =
            Date::from_calendar_date(2024, Month::January, 1).unwrap() + Duration::days(299);
        let prefix_bars = all_bars
            .iter()
            .filter(|bar| bar.date <= cutoff)
            .cloned()
            .collect::<Vec<_>>();
        let full = residual_features(&all_bars, &instruments, &BTreeMap::new()).unwrap();
        let prefix = residual_features(&prefix_bars, &instruments, &BTreeMap::new()).unwrap();
        assert_eq!(
            prefix.get(&("R0".into(), cutoff)).unwrap().values,
            full.get(&("R0".into(), cutoff)).unwrap().values
        );

        let matrix = training_matrix_for_named_feature_set(
            &all_bars,
            &instruments,
            cutoff,
            cutoff + Duration::days(20),
            5,
            0.0,
            FeatureSet::Residual,
        )
        .unwrap();
        assert!(!matrix.rows.is_empty());
        assert_eq!(matrix.features, residual_model_feature_names());
        assert!(matrix
            .rows
            .iter()
            .all(|row| row.features.len() == residual_model_feature_names().len()));
        assert!(matrix.rows.iter().all(|row| {
            let absolute = row.return_per_risk_target.unwrap();
            (absolute * row.vol60 - row.target.unwrap()).abs() < 1e-12
        }));
        for date in matrix
            .rows
            .iter()
            .map(|row| row.date)
            .collect::<BTreeSet<_>>()
        {
            let ranks = matrix
                .rows
                .iter()
                .filter(|row| row.date == date)
                .map(|row| row.relative_rank_target.unwrap())
                .collect::<Vec<_>>();
            assert!(ranks.iter().all(|rank| (-1.0..=1.0).contains(rank)));
            assert!(ranks.iter().sum::<f64>().abs() < 1e-10);
            let relative_risk = matrix
                .rows
                .iter()
                .filter(|row| row.date == date)
                .map(|row| row.relative_return_per_risk_target.unwrap())
                .sum::<f64>();
            assert!(relative_risk.abs() < 1e-10);
        }
        let beta_missing = matrix
            .features
            .iter()
            .position(|name| name == "m_beta_252")
            .unwrap();
        assert!(matrix
            .rows
            .iter()
            .all(|row| { row.features[&matrix.features[beta_missing]] == 0.0 }));
    }

    #[test]
    fn contextual_model_rows_are_entirely_rust_owned() {
        let template = daily(&bars("A", 300, 1.0)).unwrap().pop().unwrap();
        let ret21 = feature_index("ret_21").unwrap();
        let inputs = (0..20)
            .map(|index| {
                let id = format!("I{index}");
                let mut feature = template.clone();
                feature.instrument_id.clone_from(&id);
                feature.values[ret21] = Some(index as f64 / 100.0 - 0.05);
                CrossSectionInput {
                    meta: InstrumentMeta {
                        instrument_id: id,
                        symbol: format!("S{index}"),
                        isin: format!("SE{index:010}"),
                        sector: if index < 10 { "A" } else { "B" }.into(),
                        bucket: UniverseBucket::LargeCap,
                    },
                    feature,
                }
            })
            .collect::<Vec<_>>();
        let output = model_cross_section(&inputs).unwrap();
        assert_eq!(output.len(), inputs.len());
        assert!(output
            .iter()
            .all(|row| row.values.len() == model_feature_names().len()));
        let names = model_feature_names();
        let breadth = names
            .iter()
            .position(|name| name == "x_market_breadth_positive_21")
            .unwrap();
        assert!(output
            .iter()
            .all(|row| (row.values[breadth] - 0.7).abs() < 1e-12));
    }

    #[test]
    fn training_matrix_excludes_first_north() {
        let mut all_bars = Vec::new();
        let mut instruments = Vec::new();
        for index in 0..12 {
            let id = format!("I{index}");
            all_bars.extend(bars(&id, 300, 1.0 + index as f64 / 100.0));
            instruments.push(InstrumentMeta {
                instrument_id: id,
                symbol: format!("S{index}"),
                isin: format!("SE{index:010}"),
                sector: "Industrials".into(),
                bucket: if index < 6 {
                    UniverseBucket::LargeCap
                } else {
                    UniverseBucket::FirstNorth
                },
            });
        }
        let start = all_bars[260].date;
        let end = all_bars[290].date;
        let matrix = training_matrix(&all_bars, &instruments, start, end, 5, 0.0).unwrap();
        assert!(!matrix.rows.is_empty());
        assert!(matrix
            .rows
            .iter()
            .all(|row| row.bucket.is_stockholm_main_market()));
        for rows in matrix.rows.chunk_by(|left, right| left.date == right.date) {
            let relative_sum = rows
                .iter()
                .map(|row| row.relative_target.unwrap())
                .sum::<f64>();
            assert!(relative_sum.abs() < 1e-10);
            assert!(rows.iter().all(|row| {
                (row.market_target.unwrap() + row.relative_target.unwrap() - row.target.unwrap())
                    .abs()
                    < 1e-12
            }));
        }
    }
}
