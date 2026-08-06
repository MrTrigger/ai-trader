//! Everything the dashboard shows, folded out of files on disk.
//!
//! No process is asked. The bot writes its state and exits; this reads what it
//! left. That is the whole reason the dashboard cannot be a dependency of the
//! bot (spec §8): if this were unavailable, or wrong, or overloaded, the bot
//! would neither notice nor care.
//!
//! # Nothing here is stored twice
//!
//! Positions, cash, exposure and P&L are all folded out of the fill log, which
//! is the only stored truth (spec §0.7). A cached NAV would be a second opinion
//! about the account with no way to tell which one is right — and the wrong one
//! would look exactly as authoritative on a dashboard.

use std::collections::BTreeMap;
use std::path::Path;

use runner::{ControlFile, Health, RunRecord, RunStore};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;
use venue::{derive_positions, Fill, OpenOrder};

/// What the account is worth, and how it is placed.
#[derive(Debug, Clone, Serialize)]
pub struct Book {
    pub nav: String,
    pub cash: String,
    /// `nav - initial_cash`. Everything the account has made or lost.
    pub total_pnl: String,
    /// Marked-to-market on open positions.
    pub unrealised_pnl: String,
    /// Total less unrealised. Derived rather than lot-accounted, so it needs no
    /// second record of what was closed.
    pub realised_pnl: String,
    pub fees_paid: String,
    /// Σ|position notional| / NAV. Leverage, by another name.
    pub gross_exposure: String,
    /// Σ position notional / NAV. Zero is the dollar-neutral target.
    pub net_exposure: String,
    pub positions: Vec<PositionView>,
    /// Every currency and asset the account holds, with what resting orders
    /// have already claimed. `available` is separate from `total` because two
    /// resting buys must not both look affordable against the same cash.
    pub balances: Vec<BalanceView>,
    /// Accepted by the venue, not yet filled.
    pub open_orders: Vec<OpenOrderView>,
    /// Assets held with no mark. Named, not silently zeroed: a position priced
    /// at nothing is indistinguishable from no position, and the difference is
    /// the whole account.
    pub unpriced: Vec<String>,
    pub fills_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BalanceView {
    pub currency: String,
    pub total: String,
    pub available: String,
    /// `total - available`: what resting orders have claimed.
    pub reserved: String,
    pub is_quote: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenOrderView {
    pub asset: String,
    pub side: String,
    pub qty: String,
    pub limit_price: String,
    pub reserved: String,
    pub client_order_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionView {
    pub asset: String,
    pub qty: String,
    pub avg_price: String,
    pub mark: Option<String>,
    pub notional: Option<String>,
    /// Signed share of NAV. Negative is short.
    pub weight: Option<f64>,
    pub unrealised_pnl: Option<String>,
    pub side: &'static str,
}

/// One fill, flattened for display.
#[derive(Debug, Clone, Serialize)]
pub struct FillView {
    pub ts: String,
    pub asset: String,
    pub side: String,
    pub qty: String,
    pub price: String,
    pub fee: String,
    pub client_order_id: String,
}

/// The dashboard's whole payload.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub generated_at: String,
    pub control_state: String,
    pub controls: ControlFile,
    pub health: Health,
    pub book: Book,
    pub runs: Vec<RunRecord>,
    pub fills: Vec<FillView>,
    pub marks: BTreeMap<String, String>,
    /// The backtest this is supposed to be reproducing, if a record was found.
    pub expectation: Option<serde_json::Value>,
    pub live: LiveStats,
    /// Our ledger against the venue's book.
    ///
    /// The dashboard's job here is to make a stopped bot legible, and "halted"
    /// with no cause is the least legible thing it could show. Reconciliation
    /// state is the single most common reason a run refuses to trade, so it is
    /// on the page rather than only in a log.
    pub reconciliation: Option<serde_json::Value>,
    /// Whether this dashboard was given a bot to run. False means it can show
    /// everything and change nothing.
    pub controls_enabled: bool,
    /// Which venue the bot is pointed at, read from its config rather than
    /// guessed. `None` when this dashboard was not given one to read.
    pub mode: Option<ModeView>,
    /// Anything that made a number here less trustworthy than it looks.
    pub warnings: Vec<String>,
}

/// How the live account is doing, and whether that means anything yet.
///
/// `meaningful` is the field that matters. A Sharpe ratio from eleven days is
/// noise with a decimal point, and a dashboard that prints it next to a
/// backtest figure invites exactly the comparison it cannot support.
#[derive(Debug, Clone, Serialize)]
pub struct LiveStats {
    pub runs_recorded: usize,
    pub clean_runs: usize,
    pub days_live: Option<i64>,
    pub return_since_start: Option<f64>,
    pub max_drawdown: Option<f64>,
    pub sharpe: Option<f64>,
    pub meaningful: bool,
    pub note: String,
}

/// The minimum sample before live performance is worth printing.
///
/// Sixty daily observations still has an enormous standard error on Sharpe;
/// it is a floor for "not obviously meaningless", not a threshold for
/// "conclusive". The backtest ran nearly four years.
const MEANINGFUL_DAYS: i64 = 60;

/// What venue the bot will use on its next run.
#[derive(Debug, Clone, Serialize)]
pub struct ModeView {
    /// `paper`, `live-readonly` or `live`.
    pub mode: String,
    /// `hyperliquid` or `file`.
    pub feed: String,
    /// The one that matters: whether a run could lose real money.
    pub real_money: bool,
}

pub struct Inputs<'a> {
    pub state_dir: &'a Path,
    pub initial_cash: Decimal,
    pub quote_currency: &'a str,
    pub cadence_hours: i64,
    pub expectation_path: Option<&'a Path>,
    pub controls_enabled: bool,
    /// The bot's own config, so the dashboard can read which venue is in use
    /// rather than keeping a second copy of that answer.
    pub bot_config: Option<&'a Path>,
    pub run_limit: usize,
    pub fill_limit: usize,
}

pub fn build(inputs: &Inputs) -> Result<Snapshot, String> {
    let now = time::OffsetDateTime::now_utc();
    let mut warnings = Vec::new();

    let controls = ControlFile::read(&inputs.state_dir.join("controls.json"));
    let store = RunStore::new(inputs.state_dir);
    let runs = store.recent(inputs.run_limit).map_err(|e| e.to_string())?;
    let health =
        runner::health(&controls, &store, inputs.cadence_hours, now).map_err(|e| e.to_string())?;

    let marks = read_marks(&inputs.state_dir.join("marks.json"), &mut warnings);
    let fills = read_fills(&inputs.state_dir.join("venue-state.json"), &mut warnings);
    let open_orders = read_open_orders(&inputs.state_dir.join("venue-state.json"));
    let book = fold_book(
        &fills,
        &marks,
        inputs.initial_cash,
        inputs.quote_currency,
        &open_orders,
        &mut warnings,
    );

    let expectation = inputs
        .expectation_path
        .and_then(|p| match std::fs::read_to_string(p) {
            // Metrics and the strategy's own description. The left rail names
            // what is running; taking that from the committed research record
            // rather than a constant means it cannot describe last month's
            // strategy.
            Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .map(|v| {
                    serde_json::json!({
                        "metrics": v.get("metrics").cloned(),
                        "strategy": v.get("strategy").cloned(),
                        "window": v.get("window").cloned(),
                    })
                }),
            Err(_) => {
                warnings.push(format!(
                "no backtest record at {} - there is nothing to compare the live account against",
                p.display()
            ));
                None
            }
        });

    let live = live_stats(&runs, &book, inputs.initial_cash);

    // Reconciled against the venue's positions as the bot last recorded them:
    // this process holds no venue credentials and cannot ask the venue itself.
    // Comparing our ledger against a stale book would be worse than saying
    // nothing, so the ledger is compared against the same fills everything else
    // on this page is folded from.
    let mut health = health;
    let reconciliation = reconcile_view(inputs.state_dir, &fills, &mut warnings);
    // Health has to account for this. `runner::health` is about controls and
    // cadence and knows nothing of the ledger, so a dashboard showing "ok"
    // beside "reconciliation disagrees" would be telling an operator the system
    // is fine while it refuses to trade.
    if reconciliation
        .as_ref()
        .and_then(|r| r.get("agrees"))
        .and_then(|v| v.as_bool())
        == Some(false)
    {
        health.ok = false;
        health.notes.push(
            "our record and the venue disagree; no run will trade until that is settled".into(),
        );
    }

    // Newest first: the question a dashboard answers is "what just happened".
    let recent_fills: Vec<FillView> = fills
        .iter()
        .rev()
        .take(inputs.fill_limit)
        .map(|f| FillView {
            ts: stamp(f.ts),
            asset: f.asset.clone(),
            side: format!("{:?}", f.side).to_lowercase(),
            qty: f.qty.normalize().to_string(),
            price: f.price.to_string(),
            fee: f.fee.to_string(),
            client_order_id: f.client_order_id.clone(),
        })
        .collect();

    Ok(Snapshot {
        generated_at: stamp(now),
        control_state: controls.state().into(),
        controls,
        health,
        book,
        runs,
        fills: recent_fills,
        marks: marks
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect(),
        expectation,
        live,
        reconciliation,
        controls_enabled: inputs.controls_enabled,
        mode: read_mode(inputs.bot_config, &mut warnings),
        warnings,
    })
}

/// Our ledger against the book, for display only.
fn reconcile_view(
    state_dir: &Path,
    fills: &[Fill],
    warnings: &mut Vec<String>,
) -> Option<serde_json::Value> {
    let ledger = runner::Ledger::open(state_dir);
    let eps: rust_decimal::Decimal = "0.00000001".parse().ok()?;
    let positions = derive_positions(fills);
    match ledger.reconcile(&positions, fills, eps) {
        Ok(r) => {
            if !r.agrees {
                warnings.push(r.explain());
            }
            serde_json::to_value(&r).ok()
        }
        Err(e) => {
            warnings.push(format!("cannot read the order ledger: {e}"));
            None
        }
    }
}

/// The bot's own config is the authority on which venue is in use. This reads
/// it rather than keeping a copy, because two places to change the mode is one
/// place too many.
fn read_mode(path: Option<&Path>, warnings: &mut Vec<String>) -> Option<ModeView> {
    let path = path?;
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let mode = v.get("mode").and_then(|m| m.as_str()).unwrap_or("paper");
    let feed = v.get("feed").and_then(|m| m.as_str()).unwrap_or("venue");
    if feed == "file" {
        warnings.push(
            "the price feed is a static file, so marks never move. A paper run against frozen \
             prices produces no P&L, no slippage and no drawdown - none of what a forward test \
             measures. Set \"feed\": \"venue\" in the bot config for live prices."
                .into(),
        );
    }
    Some(ModeView {
        mode: mode.to_string(),
        feed: feed.to_string(),
        real_money: mode == "live",
    })
}

fn stamp(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| t.unix_timestamp().to_string())
}

fn read_marks(path: &Path, warnings: &mut Vec<String>) -> BTreeMap<String, Decimal> {
    let Ok(text) = std::fs::read_to_string(path) else {
        warnings.push(format!(
            "no marks at {} - positions cannot be valued, so NAV is cash only",
            path.display()
        ));
        return BTreeMap::new();
    };
    match serde_json::from_str::<BTreeMap<String, String>>(&text) {
        Ok(raw) => raw
            .into_iter()
            .filter_map(|(k, v)| v.parse::<Decimal>().ok().map(|d| (k, d)))
            .collect(),
        Err(e) => {
            warnings.push(format!("marks at {} are unreadable: {e}", path.display()));
            BTreeMap::new()
        }
    }
}

fn read_fills(path: &Path, warnings: &mut Vec<String>) -> Vec<Fill> {
    let Ok(text) = std::fs::read_to_string(path) else {
        warnings.push(format!(
            "no venue state at {} - nothing has traded yet, or the bot has not run here",
            path.display()
        ));
        return Vec::new();
    };
    match paper::fills_in_snapshot(&text) {
        Ok(f) => f,
        Err(e) => {
            warnings.push(format!(
                "venue state at {} could not be read ({e}); the book shown here is empty and \
                 that is a display failure, not a flat account",
                path.display()
            ));
            Vec::new()
        }
    }
}

/// Resting orders, or none if the venue state is missing. A missing file is
/// already reported by `read_fills`; saying it twice adds noise, not detail.
fn read_open_orders(path: &Path) -> Vec<OpenOrder> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| paper::open_orders_in_snapshot(&t).ok())
        .unwrap_or_default()
}

fn fold_book(
    fills: &[Fill],
    marks: &BTreeMap<String, Decimal>,
    initial_cash: Decimal,
    quote_currency: &str,
    open_orders: &[OpenOrder],
    warnings: &mut Vec<String>,
) -> Book {
    let cash = fills.iter().fold(initial_cash, |acc, f| {
        acc - f.signed_qty() * f.price - f.fee
    });
    let fees: Decimal = fills.iter().map(|f| f.fee).sum();

    let mut positions = Vec::new();
    let mut notionals = Vec::new();
    let mut unpriced = Vec::new();
    let mut long_notional = Decimal::ZERO;
    let mut short_notional = Decimal::ZERO;
    let mut unrealised = Decimal::ZERO;

    for p in derive_positions(fills) {
        if p.qty.is_zero() {
            continue;
        }
        let mark = marks.get(&p.asset).copied();
        let notional = mark.map(|m| p.qty * m);
        if let Some(n) = notional {
            if n.is_sign_positive() {
                long_notional += n;
            } else {
                short_notional += n;
            }
            unrealised += p.qty * (mark.unwrap() - p.avg_price);
        } else {
            unpriced.push(p.asset.clone());
        }
        notionals.push(notional);
        positions.push(PositionView {
            asset: p.asset.clone(),
            qty: p.qty.normalize().to_string(),
            avg_price: p.avg_price.to_string(),
            mark: mark.map(|m| m.to_string()),
            notional: notional.map(money),
            weight: None, // filled below, once NAV is known
            unrealised_pnl: mark.map(|m| money(p.qty * (m - p.avg_price))),
            side: if p.qty.is_sign_positive() {
                "long"
            } else {
                "short"
            },
        });
    }

    if !unpriced.is_empty() {
        warnings.push(format!(
            "no mark for {} - those positions are excluded from NAV and exposure rather than \
             valued at zero",
            unpriced.join(", ")
        ));
    }

    let nav = cash + long_notional + short_notional;
    let gross = long_notional - short_notional;

    // Weight needs NAV, and NAV needs every notional, so it cannot be filled in
    // during the loop above.
    if !nav.is_zero() {
        for (view, notional) in positions.iter_mut().zip(&notionals) {
            view.weight = notional.and_then(|n| (n / nav).to_f64());
        }
    }

    // What resting orders have claimed, by currency. Folded from the orders
    // themselves rather than stored: a reservation total kept alongside them
    // is a second opinion that can disagree.
    let mut reserved: BTreeMap<String, Decimal> = BTreeMap::new();
    for o in open_orders {
        *reserved.entry(o.reserved_currency.clone()).or_default() += o.reserved;
    }
    let claim = |c: &str| reserved.get(c).copied().unwrap_or_default();

    let mut balances = vec![BalanceView {
        currency: quote_currency.to_string(),
        total: money(cash),
        available: money(cash - claim(quote_currency)),
        reserved: money(claim(quote_currency)),
        is_quote: true,
    }];
    for p in derive_positions(fills)
        .into_iter()
        .filter(|p| !p.qty.is_zero())
    {
        let c = claim(&p.asset);
        balances.push(BalanceView {
            currency: p.asset.clone(),
            total: p.qty.normalize().to_string(),
            available: (p.qty - c).normalize().to_string(),
            reserved: c.normalize().to_string(),
            is_quote: false,
        });
    }

    Book {
        nav: money(nav),
        cash: money(cash),
        total_pnl: money(nav - initial_cash),
        unrealised_pnl: money(unrealised),
        realised_pnl: money(nav - initial_cash - unrealised),
        fees_paid: money(fees),
        gross_exposure: pct_of(gross, nav),
        net_exposure: pct_of(long_notional + short_notional, nav),
        positions,
        balances,
        open_orders: open_orders
            .iter()
            .map(|o| OpenOrderView {
                asset: o.asset.clone(),
                side: format!("{:?}", o.side).to_lowercase(),
                qty: o.qty.normalize().to_string(),
                limit_price: o.limit_price.to_string(),
                reserved: o.reserved.normalize().to_string(),
                client_order_id: o.client_order_id.clone(),
            })
            .collect(),
        unpriced,
        fills_count: fills.len(),
    }
}

/// Money, always to the cent.
///
/// `round_dp` alone drops trailing zeros, so 899 and 899.00 come out looking
/// like different kinds of number in a column that is meant to line up.
fn money(d: Decimal) -> String {
    format!("{:.2}", d)
}

fn pct_of(value: Decimal, nav: Decimal) -> String {
    if nav.is_zero() {
        return "n/a".into();
    }
    format!("{:.4}", value / nav)
}

/// Live performance from the NAV each run recorded, plus where the book is now.
///
/// This is the planner's view of NAV at decision time, not a continuous equity
/// curve — the account is only marked when something runs. Said plainly here
/// because a chart of it looks exactly like a continuous one.
fn live_stats(runs: &[RunRecord], book: &Book, initial_cash: Decimal) -> LiveStats {
    let clean = runs.iter().filter(|r| r.is_clean()).count();
    let navs: Vec<f64> = runs
        .iter()
        .rev()
        .filter_map(|r| r.nav.as_ref().and_then(|n| n.parse::<f64>().ok()))
        .collect();

    let days = first_and_last(runs).map(|(a, b)| (b - a).whole_days());
    let start = initial_cash.to_f64().unwrap_or(0.0);
    let current = book.nav.parse::<f64>().unwrap_or(start);
    let ret = (start > 0.0).then(|| current / start - 1.0);

    let meaningful = days.unwrap_or(0) >= MEANINGFUL_DAYS && navs.len() >= 30;
    let note = if navs.len() < 2 {
        "Nothing has run yet, so there is no live record to compare against anything.".into()
    } else if !meaningful {
        format!(
            "{} runs over {} days, against a backtest of several years. Far too short to judge \
             anything: a Sharpe ratio from this sample is noise with a decimal point, and even \
             the drawdown is mostly a statement about which day it is. These figures are here so \
             the account can be watched, not so the strategy can be assessed.",
            runs.len(),
            days.unwrap_or(0)
        )
    } else {
        format!(
            "{} runs over {} days. Long enough to be worth reading, and still a fraction of the \
             backtest window.",
            runs.len(),
            days.unwrap_or(0)
        )
    };

    LiveStats {
        runs_recorded: runs.len(),
        clean_runs: clean,
        days_live: days,
        return_since_start: ret,
        max_drawdown: max_drawdown(&navs),
        sharpe: meaningful.then(|| sharpe(&navs)).flatten(),
        meaningful,
        note,
    }
}

fn first_and_last(runs: &[RunRecord]) -> Option<(time::OffsetDateTime, time::OffsetDateTime)> {
    let parse = |s: &String| {
        time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
    };
    let last = runs.first().and_then(|r| parse(&r.recorded_at))?;
    let first = runs.last().and_then(|r| parse(&r.recorded_at))?;
    Some((first, last))
}

fn max_drawdown(navs: &[f64]) -> Option<f64> {
    if navs.len() < 2 {
        return None;
    }
    let mut peak = navs[0];
    let mut worst: f64 = 0.0;
    for &v in navs {
        peak = peak.max(v);
        if peak > 0.0 {
            worst = worst.min(v / peak - 1.0);
        }
    }
    Some(worst)
}

/// Annualised on the run cadence, which is daily.
///
/// No flat-run skipping. Phase 1 found that dropping stand-down periods and
/// then annualising by the full count inflated Sharpe by 15-23%, and the same
/// mistake is available here for free.
fn sharpe(navs: &[f64]) -> Option<f64> {
    let rets: Vec<f64> = navs
        .windows(2)
        .filter(|w| w[0] > 0.0)
        .map(|w| w[1] / w[0] - 1.0)
        .collect();
    if rets.len() < 30 {
        return None;
    }
    let n = rets.len() as f64;
    let mean = rets.iter().sum::<f64>() / n;
    let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let sd = var.sqrt();
    (sd > 0.0).then(|| mean / sd * 365f64.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use venue::Side;

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn fill(asset: &str, side: Side, qty: &str, price: &str, fee: &str) -> Fill {
        Fill {
            venue_fill_id: format!("f{asset}{qty}"),
            venue_order_id: "v".into(),
            client_order_id: "c".into(),
            asset: asset.into(),
            side,
            qty: dec(qty),
            price: dec(price),
            fee: dec(fee),
            fee_currency: "USD".into(),
            ts: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// `fold_book` with the arguments a test never varies.
    fn book(
        fills: &[Fill],
        marks: &BTreeMap<String, Decimal>,
        initial_cash: Decimal,
        w: &mut Vec<String>,
    ) -> Book {
        fold_book(fills, marks, initial_cash, "USD", &[], w)
    }

    fn marks(pairs: &[(&str, &str)]) -> BTreeMap<String, Decimal> {
        pairs
            .iter()
            .map(|(a, p)| ((*a).to_string(), dec(p)))
            .collect()
    }

    #[test]
    fn nav_is_cash_plus_marked_positions_and_pnl_is_nav_less_what_went_in() {
        // Buy 1 BTC at 100 with a 1 fee, mark it at 110.
        let fills = vec![fill("BTC", Side::Buy, "1", "100", "1")];
        let mut w = Vec::new();
        let b = book(&fills, &marks(&[("BTC", "110")]), dec("1000"), &mut w);
        assert_eq!(b.cash, "899.00"); // 1000 - 100 - 1
        assert_eq!(b.nav, "1009.00"); // 899 + 110
        assert_eq!(b.total_pnl, "9.00");
        assert_eq!(b.unrealised_pnl, "10.00");
        assert_eq!(b.realised_pnl, "-1.00", "the fee is the only realised cost");
        assert_eq!(b.fees_paid, "1.00");
        assert!(w.is_empty());
    }

    #[test]
    fn a_short_reduces_net_exposure_and_adds_to_gross() {
        let fills = vec![
            fill("BTC", Side::Buy, "1", "100", "0"),
            fill("ETH", Side::Sell, "10", "10", "0"),
        ];
        let mut w = Vec::new();
        let b = book(
            &fills,
            &marks(&[("BTC", "100"), ("ETH", "10")]),
            dec("1000"),
            &mut w,
        );
        // cash 1000 - 100 + 100 = 1000; positions +100 and -100; NAV 1000.
        assert_eq!(b.nav, "1000.00");
        assert_eq!(b.net_exposure, "0.0000", "long and short cancel");
        assert_eq!(b.gross_exposure, "0.2000", "200 of exposure on 1000 of NAV");
    }

    #[test]
    fn a_position_with_no_mark_is_named_rather_than_valued_at_zero() {
        // The failure this prevents: an unpriced holding silently valued at
        // nothing looks exactly like no holding at all.
        let fills = vec![fill("DOGE", Side::Buy, "1000", "1", "0")];
        let mut w = Vec::new();
        let b = book(&fills, &marks(&[]), dec("5000"), &mut w);
        assert_eq!(b.unpriced, vec!["DOGE".to_string()]);
        assert_eq!(b.positions.len(), 1);
        assert!(b.positions[0].mark.is_none());
        assert!(b.positions[0].notional.is_none());
        assert!(
            w.iter().any(|m| m.contains("DOGE")),
            "the operator has to be told: {w:?}"
        );
    }

    #[test]
    fn a_closed_position_is_not_listed() {
        let fills = vec![
            fill("BTC", Side::Buy, "1", "100", "0"),
            fill("BTC", Side::Sell, "1", "120", "0"),
        ];
        let mut w = Vec::new();
        let b = book(&fills, &marks(&[("BTC", "120")]), dec("1000"), &mut w);
        assert!(b.positions.is_empty(), "flat is flat");
        assert_eq!(b.total_pnl, "20.00");
        assert_eq!(b.unrealised_pnl, "0.00");
        assert_eq!(b.realised_pnl, "20.00");
    }

    #[test]
    fn weights_are_signed_shares_of_nav() {
        let fills = vec![fill("BTC", Side::Buy, "1", "100", "0")];
        let mut w = Vec::new();
        let b = book(&fills, &marks(&[("BTC", "100")]), dec("1000"), &mut w);
        assert!((b.positions[0].weight.unwrap() - 0.1).abs() < 1e-9);

        let short = vec![fill("BTC", Side::Sell, "1", "100", "0")];
        let b = book(&short, &marks(&[("BTC", "100")]), dec("1000"), &mut w);
        assert!(b.positions[0].weight.unwrap() < 0.0, "short is negative");
    }

    #[test]
    fn a_sharpe_is_not_reported_from_a_short_sample() {
        // The whole point of the flag. A ratio from a handful of days is noise
        // with a decimal point, and printing it beside a backtest figure invites
        // the comparison it cannot support.
        let navs: Vec<f64> = (0..10).map(|i| 1000.0 + i as f64).collect();
        assert!(sharpe(&navs).is_none());

        let long: Vec<f64> = (0..200).map(|i| 1000.0 * 1.001f64.powi(i)).collect();
        assert!(sharpe(&long).is_some());
    }

    #[test]
    fn drawdown_is_measured_from_the_running_peak() {
        assert_eq!(max_drawdown(&[100.0, 120.0, 60.0, 90.0]), Some(-0.5));
        assert_eq!(max_drawdown(&[100.0, 110.0, 120.0]), Some(0.0));
        assert_eq!(max_drawdown(&[100.0]), None);
    }
}
