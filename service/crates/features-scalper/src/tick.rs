//! fs-5 tick order-flow features (indices 38–49) - `docs/scalper-research.md`
//! Amendment 5, §5.2. Every definition here is the amendment's table,
//! implemented once, with no knob.
//!
//! Input discipline mirrors `MicroMinute`: the CALLER hands `compute` a
//! [`TapeSource`] that answers, for a bar with open time `T`, the trades of
//! `[T, T+60 s)` in archive order (`Some(TapeMinute)`), or `None` when no
//! tape day covers that minute. This module never opens a file.
//!
//! Windows. A bar is evaluated at its close `C = T + 60 s`; a window of
//! length `W` is `[C − W, C)` in `ts_ms` - strictly before the close (a
//! trade stamped exactly `C` belongs to the NEXT bar's minute and is never
//! visible here). The 10 s and 30 s windows lie inside the bar's own minute;
//! the 5-minute window spans the bar's minute and the four before it; the
//! 60-minute baseline (features 46/47) spans sixty. A window is available
//! only if every minute it touches is `Some` and the minutes are
//! consecutive (60 s apart): a `None` minute, or a bar-series gap that
//! skips a minute, breaks coverage and the deques restart from empty. This
//! is the tape analogue of the crate's full-and-clean rolling-window rule.
//!
//! Order. "Last trade" and "run" follow archive row order (aggTrade id
//! order = execution order); window membership follows `ts_ms`.
//!
//! `None` conditions are exactly the table's; the crate-wide finite sweep
//! in `compute` still applies on top.

use std::collections::VecDeque;

/// One aggTrade as the feature crate sees it. `is_buy` = the taker bought
/// (Binance `is_buyer_maker == false`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TapeTrade {
    pub ts_ms: i64,
    pub price: f64,
    pub qty: f64,
    pub is_buy: bool,
}

impl TapeTrade {
    pub fn notional(&self) -> f64 {
        self.price * self.qty
    }
    fn signed_notional(&self) -> f64 {
        if self.is_buy {
            self.notional()
        } else {
            -self.notional()
        }
    }
}

/// The trades of one bar's minute `[T, T+60 s)`, archive order. An empty
/// `trades` is a covered minute with no prints - distinct from `None` at
/// the [`TapeSource`] level, which means "no tape for this minute".
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TapeMinute {
    pub trades: Vec<TapeTrade>,
}

/// Caller-provided tape access, consumed in ascending bar order.
pub trait TapeSource {
    /// Trades of `[minute_ts_s, minute_ts_s + 60)`, or `None` if no tape day
    /// covers that minute.
    fn minute(&mut self, minute_ts_s: i64) -> Result<Option<TapeMinute>, String>;
}

/// The "no tape at all" source: every tick feature is `None`, the other 38
/// are untouched - fs-4 parity.
pub struct NoTape;

impl TapeSource for NoTape {
    fn minute(&mut self, _minute_ts_s: i64) -> Result<Option<TapeMinute>, String> {
        Ok(None)
    }
}

/// A deterministic tape for tests and fixtures in this and other crates:
/// every minute is covered and carries six trades, one per 10-second
/// bucket, alternating buy/sell with a slow price ramp - enough that all
/// twelve tick features are `Some` once sixty consecutive minutes have been
/// served. Not a model of any market; a fixture.
pub struct SyntheticTape;

impl SyntheticTape {
    pub fn minute_trades(minute_ts_s: i64) -> Vec<TapeTrade> {
        (0..6)
            .map(|j| {
                let ts_ms = minute_ts_s * 1000 + j * 10_000 + 1;
                let price = 100.0 + ((minute_ts_s / 60) % 1000) as f64 * 0.001 + j as f64 * 0.0001;
                TapeTrade {
                    ts_ms,
                    price,
                    qty: 1.0 + j as f64,
                    is_buy: j % 2 == 0,
                }
            })
            .collect()
    }
}

impl TapeSource for SyntheticTape {
    fn minute(&mut self, minute_ts_s: i64) -> Result<Option<TapeMinute>, String> {
        Ok(Some(TapeMinute {
            trades: Self::minute_trades(minute_ts_s),
        }))
    }
}

pub const TICK_FEATURE_NAMES: [&str; 12] = [
    "tk_imb_10s",
    "tk_imb_30s",
    "tk_imb_5m",
    "tk_large_imb_5m",
    "tk_run",
    "tk_ret_10s",
    "tk_ret_30s",
    "tk_vwap_dev_30s",
    "tk_intensity_10s",
    "tk_notional_ratio_30s",
    "tk_impact_5m",
    "tk_size_med_5m_log",
];

const MS_10S: i64 = 10_000;
const MS_30S: i64 = 30_000;
const MS_5M: i64 = 300_000;
const MINUTES_5M: usize = 5;
const MINUTES_60M: usize = 60;
const RUN_CAP: i64 = 50;
const LARGE_MIN_TRADES: usize = 10;
const IMPACT_BUCKETS: usize = 30;
const IMPACT_MIN_ACTIVE_BUCKETS: usize = 10;

/// Rolling tape state for one asset, advanced once per bar.
#[derive(Default)]
pub struct TickState {
    /// Up to 5 most recent covered minutes, `(minute_ts_s, minute)`,
    /// consecutive by construction.
    recent: VecDeque<(i64, TapeMinute)>,
    /// Up to 60 most recent covered minutes' `(minute_ts_s, n_trades,
    /// notional)`, consecutive by construction (a superset in time of
    /// `recent`).
    baseline: VecDeque<(i64, u64, f64)>,
}

impl TickState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance to the bar with open time `minute_ts_s` and return the twelve
    /// features at its close. `minute == None` (no tape) breaks coverage and
    /// yields twelve `None`s.
    pub fn push(&mut self, minute_ts_s: i64, minute: Option<TapeMinute>) -> [Option<f64>; 12] {
        let Some(minute) = minute else {
            self.recent.clear();
            self.baseline.clear();
            return [None; 12];
        };
        let contiguous = match self.recent.back() {
            Some((prev, _)) => *prev + 60 == minute_ts_s,
            None => true,
        };
        if !contiguous {
            self.recent.clear();
            self.baseline.clear();
        }
        let n = minute.trades.len() as u64;
        let notional: f64 = minute.trades.iter().map(TapeTrade::notional).sum();
        self.recent.push_back((minute_ts_s, minute));
        while self.recent.len() > MINUTES_5M {
            self.recent.pop_front();
        }
        self.baseline.push_back((minute_ts_s, n, notional));
        while self.baseline.len() > MINUTES_60M {
            self.baseline.pop_front();
        }

        let close_ms = (minute_ts_s + 60) * 1000;
        let cur: &[TapeTrade] = &self.recent.back().expect("just pushed").1.trades;
        let mut out = [None; 12];

        // --- 10 s / 30 s windows: inside the current minute ---
        let w10: Vec<&TapeTrade> = cur
            .iter()
            .filter(|t| t.ts_ms >= close_ms - MS_10S && t.ts_ms < close_ms)
            .collect();
        let w30: Vec<&TapeTrade> = cur
            .iter()
            .filter(|t| t.ts_ms >= close_ms - MS_30S && t.ts_ms < close_ms)
            .collect();
        out[0] = imbalance(w10.iter().copied());
        out[1] = imbalance(w30.iter().copied());

        // --- 5-minute window: needs five consecutive covered minutes ---
        let have_5m = self.recent.len() == MINUTES_5M;
        let w5m: Vec<&TapeTrade> = if have_5m {
            self.recent
                .iter()
                .flat_map(|(_, m)| m.trades.iter())
                .filter(|t| t.ts_ms >= close_ms - MS_5M && t.ts_ms < close_ms)
                .collect()
        } else {
            Vec::new()
        };
        if have_5m {
            out[2] = imbalance(w5m.iter().copied());
            out[3] = large_imbalance(&w5m);
            out[4] = run_length(&w5m);
            let p_last = w5m.last().map(|t| t.price);
            out[5] = ret_bps(p_last, last_price_before(&w5m, close_ms - MS_10S));
            out[6] = ret_bps(p_last, last_price_before(&w5m, close_ms - MS_30S));
            out[10] = impact(&w5m, close_ms);
            out[11] = size_med_log(&w5m);
        }
        // vwap deviation needs only the 30 s window: its last trade IS the
        // last trade before the close.
        out[7] = vwap_dev(w30.last().map(|t| t.price), &w30);

        // --- 60-minute baseline ---
        if self.baseline.len() == MINUTES_60M {
            let n_60m: u64 = self.baseline.iter().map(|(_, n, _)| *n).sum();
            let notional_60m: f64 = self.baseline.iter().map(|(_, _, v)| *v).sum();
            let n_10s = w10.len() as f64;
            let notional_30s: f64 = w30.iter().map(|t| t.notional()).sum();
            out[8] = Some(((n_10s + 1.0) / (n_60m as f64 / 360.0 + 1.0)).ln());
            out[9] = Some(((notional_30s + 1.0) / (notional_60m / 120.0 + 1.0)).ln());
        }

        out
    }
}

fn imbalance<'a>(trades: impl Iterator<Item = &'a TapeTrade>) -> Option<f64> {
    let (mut buy, mut sell) = (0.0, 0.0);
    let mut any = false;
    for t in trades {
        any = true;
        if t.is_buy {
            buy += t.notional();
        } else {
            sell += t.notional();
        }
    }
    if !any {
        return None;
    }
    let denom = buy + sell;
    if denom <= 0.0 {
        return None;
    }
    Some((buy - sell) / denom)
}

/// Same interpolation as `scalper_data::costs::percentile` (Amendment 5
/// binds `tk_large_imb_5m`'s p90 to it): rank `p·(n−1)`, linear between
/// the two nearest order statistics.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let rank = p * (n - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = rank - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

fn large_imbalance(w5m: &[&TapeTrade]) -> Option<f64> {
    if w5m.len() < LARGE_MIN_TRADES {
        return None;
    }
    let notionals: Vec<f64> = w5m.iter().map(|t| t.notional()).collect();
    let p90 = percentile(&notionals, 0.9);
    imbalance(w5m.iter().copied().filter(|t| t.notional() >= p90))
}

fn run_length(w5m: &[&TapeTrade]) -> Option<f64> {
    let last = w5m.last()?;
    let mut n: i64 = 0;
    for t in w5m.iter().rev() {
        if t.is_buy != last.is_buy || n >= RUN_CAP {
            break;
        }
        n += 1;
    }
    Some(if last.is_buy { n as f64 } else { -(n as f64) })
}

/// Price of the last trade (archive order) with `ts_ms < before_ms`.
fn last_price_before(w: &[&TapeTrade], before_ms: i64) -> Option<f64> {
    w.iter()
        .rev()
        .find(|t| t.ts_ms < before_ms)
        .map(|t| t.price)
}

fn ret_bps(p_last: Option<f64>, p_ref: Option<f64>) -> Option<f64> {
    let (a, b) = (p_last?, p_ref?);
    if a <= 0.0 || b <= 0.0 {
        return None;
    }
    Some(1e4 * (a / b).ln())
}

fn vwap_dev(p_last: Option<f64>, w30: &[&TapeTrade]) -> Option<f64> {
    if w30.is_empty() {
        return None;
    }
    let p_last = p_last?;
    let notional: f64 = w30.iter().map(|t| t.notional()).sum();
    let qty: f64 = w30.iter().map(|t| t.qty).sum();
    if qty <= 0.0 {
        return None;
    }
    let vwap = notional / qty;
    Some(1e4 * (p_last - vwap) / vwap)
}

/// Kyle-λ proxy over the thirty 10-second buckets of `[C − 300 s, C)`:
/// `x_i` = signed notional of bucket `i`, `y_i = 1e4·ln(last_i / last_{i−1})`
/// with an empty bucket inheriting the prior bucket's last price. Bucket 0
/// has no `last_{−1}` inside the window, so pairs are formed for `i ≥ 1`
/// wherever both lasts are defined; feature = `cov(x, y)/var(x) × 1e6`
/// (bps per $1M signed), `None` if fewer than 10 buckets contain a trade
/// or `var(x) = 0`.
fn impact(w5m: &[&TapeTrade], close_ms: i64) -> Option<f64> {
    let start = close_ms - MS_5M;
    let mut x = [0.0f64; IMPACT_BUCKETS];
    let mut last: [Option<f64>; IMPACT_BUCKETS] = [None; IMPACT_BUCKETS];
    let mut active = [false; IMPACT_BUCKETS];
    for t in w5m {
        let i = ((t.ts_ms - start) / MS_10S) as usize;
        if i >= IMPACT_BUCKETS {
            continue; // cannot happen inside the window; defensive
        }
        x[i] += t.signed_notional();
        last[i] = Some(t.price);
        active[i] = true;
    }
    if active.iter().filter(|a| **a).count() < IMPACT_MIN_ACTIVE_BUCKETS {
        return None;
    }
    // Inherit forward.
    for i in 1..IMPACT_BUCKETS {
        if last[i].is_none() {
            last[i] = last[i - 1];
        }
    }
    let mut xs = Vec::with_capacity(IMPACT_BUCKETS);
    let mut ys = Vec::with_capacity(IMPACT_BUCKETS);
    for i in 1..IMPACT_BUCKETS {
        if let (Some(a), Some(b)) = (last[i], last[i - 1]) {
            if a > 0.0 && b > 0.0 {
                xs.push(x[i]);
                ys.push(1e4 * (a / b).ln());
            }
        }
    }
    if xs.len() < 2 {
        return None;
    }
    let n = xs.len() as f64;
    let mx = xs.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let mut cov = 0.0;
    let mut var = 0.0;
    for (a, b) in xs.iter().zip(ys.iter()) {
        cov += (a - mx) * (b - my);
        var += (a - mx) * (a - mx);
    }
    if var <= 0.0 {
        return None;
    }
    Some(cov / var * 1e6)
}

fn size_med_log(w5m: &[&TapeTrade]) -> Option<f64> {
    if w5m.is_empty() {
        return None;
    }
    let notionals: Vec<f64> = w5m.iter().map(|t| t.notional()).collect();
    Some((percentile(&notionals, 0.5) + 1.0).ln())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ts_ms: i64, price: f64, qty: f64, is_buy: bool) -> TapeTrade {
        TapeTrade {
            ts_ms,
            price,
            qty,
            is_buy,
        }
    }

    /// A minute at open `m` (seconds) with the given trades (already
    /// stamped inside `[m, m+60)`).
    fn minute(trades: Vec<TapeTrade>) -> Option<TapeMinute> {
        Some(TapeMinute { trades })
    }

    const M0: i64 = 1_700_000_000 - (1_700_000_000 % 60); // a minute boundary

    #[test]
    fn a_trade_stamped_exactly_at_the_close_is_not_visible() {
        // The minute [M0, M0+60): a trade at M0+60 s belongs to the next
        // minute and the caller would never put it in this TapeMinute; but
        // even inside a wrongly-assembled minute the ts filter is strict.
        let mut s = TickState::new();
        let c = (M0 + 60) * 1000;
        let f = s.push(
            M0,
            minute(vec![
                t(c - 5_000, 100.0, 1.0, true),
                t(c, 100.0, 1.0, false),
            ]),
        );
        // Only the buy is inside [C-10s, C): imbalance +1, not 0.
        assert_eq!(f[0], Some(1.0));
    }

    #[test]
    fn imbalance_windows_and_none_when_empty() {
        let mut s = TickState::new();
        let c = (M0 + 60) * 1000;
        let f = s.push(
            M0,
            minute(vec![
                t(c - 50_000, 100.0, 2.0, false), // outside 30 s: sell 200
                t(c - 20_000, 100.0, 1.0, true),  // inside 30 s: buy 100
                t(c - 5_000, 100.0, 3.0, true),   // inside 10 s: buy 300
            ]),
        );
        assert_eq!(f[0], Some(1.0)); // 10 s: only buys
        assert_eq!(f[1], Some(1.0)); // 30 s: buys 400, no sells
                                     // 5-minute window not yet covered (only one minute pushed).
        assert!(f[2].is_none() && f[3].is_none() && f[4].is_none());
        // Empty 10 s window -> None, not 0.
        let f2 = s.push(
            M0 + 60,
            minute(vec![t(c + 60_000 - 40_000, 100.0, 1.0, true)]),
        );
        assert!(f2[0].is_none());
        assert_eq!(f2[1], None); // 40 s ago is outside 30 s too
    }

    #[test]
    fn five_minute_features_need_five_consecutive_covered_minutes() {
        let mut s = TickState::new();
        let mut f = [None; 12];
        for k in 0..5 {
            let m = M0 + 60 * k;
            let c = (m + 60) * 1000;
            f = s.push(
                m,
                minute(vec![t(c - 30_000, 100.0 + k as f64, 1.0, k % 2 == 0)]),
            );
            if k < 4 {
                assert!(f[2].is_none(), "minute {k} should not have 5m coverage yet");
            }
        }
        assert!(f[2].is_some());
        // Buys at k=0,2,4 (100,102,104 notional-ish), sells at 1,3.
        // A None minute breaks coverage...
        let f = s.push(M0 + 300, None);
        assert!(f.iter().all(Option::is_none));
        // ...and so does a skipped minute (bar gap of one minute).
        for k in 6..10 {
            let m = M0 + 60 * k;
            let c = (m + 60) * 1000;
            s.push(m, minute(vec![t(c - 1_000, 100.0, 1.0, true)]));
        }
        // 4 minutes since the break: not covered.
        let m = M0 + 60 * 10;
        let c = (m + 60) * 1000;
        let f = s.push(m, minute(vec![t(c - 1_000, 100.0, 1.0, true)]));
        assert!(
            f[2].is_some(),
            "5 consecutive minutes (6..=10) restore coverage"
        );
        // Skip minute 11, push 12: coverage broken again.
        let m = M0 + 60 * 12;
        let c = (m + 60) * 1000;
        let f = s.push(m, minute(vec![t(c - 1_000, 100.0, 1.0, true)]));
        assert!(f[2].is_none());
    }

    #[test]
    fn run_is_signed_and_clipped_at_fifty() {
        let mut s = TickState::new();
        for k in 0..4 {
            let m = M0 + 60 * k;
            s.push(m, minute(vec![]));
        }
        let m = M0 + 240;
        let c = (m + 60) * 1000;
        let mut trades = vec![t(c - 59_000, 100.0, 1.0, true)];
        for j in 0..70 {
            trades.push(t(c - 50_000 + j * 100, 100.0, 1.0, false));
        }
        let f = s.push(m, minute(trades));
        assert_eq!(f[4], Some(-50.0));
    }

    #[test]
    fn sub_minute_returns_use_last_prices_before_the_reference_instant() {
        let mut s = TickState::new();
        for k in 0..4 {
            s.push(M0 + 60 * k, minute(vec![]));
        }
        let m = M0 + 240;
        let c = (m + 60) * 1000;
        let f = s.push(
            m,
            minute(vec![
                t(c - 40_000, 100.0, 1.0, true), // last before C-30s
                t(c - 15_000, 101.0, 1.0, true), // last before C-10s
                t(c - 2_000, 102.0, 1.0, true),  // last
            ]),
        );
        let exp10 = 1e4 * (102.0f64 / 101.0).ln();
        let exp30 = 1e4 * (102.0f64 / 100.0).ln();
        assert!((f[5].unwrap() - exp10).abs() < 1e-9);
        assert!((f[6].unwrap() - exp30).abs() < 1e-9);
        // vwap over 30 s: trades at 101 and 102, qty 1 each -> 101.5
        let expv = 1e4 * (102.0 - 101.5) / 101.5;
        assert!((f[7].unwrap() - expv).abs() < 1e-9);
        // No trade before C-30s in the window -> ret_30s None.
        let m2 = m + 60;
        let c2 = (m2 + 60) * 1000;
        let f2 = s.push(m2, minute(vec![t(c2 - 20_000, 103.0, 1.0, true)]));
        // p_ref for 30 s: last trade with ts < c2-30s -> that is 102.0 from
        // the previous minute (still inside the 5m window), so Some.
        assert!(f2[6].is_some());
        assert!((f2[6].unwrap() - 1e4 * (103.0f64 / 102.0).ln()).abs() < 1e-9);
    }

    #[test]
    fn large_print_imbalance_uses_the_windows_p90() {
        let mut s = TickState::new();
        for k in 0..4 {
            s.push(M0 + 60 * k, minute(vec![]));
        }
        let m = M0 + 240;
        let c = (m + 60) * 1000;
        // 9 small buys + 1 huge sell -> fewer than 10 -> None; add one more.
        let mut trades: Vec<TapeTrade> = (0..9)
            .map(|j| t(c - 30_000 + j, 100.0, 1.0, true))
            .collect();
        trades.push(t(c - 1_000, 100.0, 1000.0, false));
        let f = s.push(m, minute(trades.clone()));
        assert!(f[3].is_some(), "10 trades is enough");
        // p90 with rank 0.9*9 = 8.1 -> between the 9th smallest (100) and
        // the largest (100000): 100 + 0.1*(99900) = 10090 -> only the huge
        // sell qualifies -> imbalance -1.
        assert_eq!(f[3], Some(-1.0));
        assert_eq!(f[2].unwrap().signum(), -1.0);
    }

    #[test]
    fn baseline_ratios_need_sixty_covered_minutes() {
        let mut s = TickState::new();
        let mut f = [None; 12];
        for k in 0..60 {
            let m = M0 + 60 * k;
            let c = (m + 60) * 1000;
            // 6 trades per minute, one per 10 s bucket, notional 100 each.
            let trades: Vec<TapeTrade> = (0..6)
                .map(|j| t(c - 60_000 + j * 10_000 + 1, 100.0, 1.0, true))
                .collect();
            f = s.push(m, minute(trades));
            if k < 59 {
                assert!(f[8].is_none());
            }
        }
        // n_60m = 360 -> expected per 10 s = 1; n_10s = 1 -> ln(2/2) = 0.
        assert!((f[8].unwrap() - 0.0).abs() < 1e-12);
        // notional_60m = 36000 -> /120 = 300; notional_30s = 300 -> ln(301/301) = 0.
        assert!((f[9].unwrap() - 0.0).abs() < 1e-12);
    }

    #[test]
    fn impact_recovers_a_planted_slope_and_is_none_when_flat() {
        let mut s = TickState::new();
        for k in 0..4 {
            s.push(M0 + 60 * k, minute(vec![]));
        }
        let m = M0 + 240;
        let c = (m + 60) * 1000;
        // Bucket i (i = 0..5 within this minute; earlier minutes empty):
        // signed notional alternates ±100, price moves ln-proportionally by
        // 2 bps per $100 signed -> slope 2 bps per $100 = 2e4 bps per $1M...
        // Build directly: price_i = price_{i-1} * exp(x_i * 2e-4 / 1e4)
        // i.e. y_i = x_i * 2e-4 bps -> lambda = 2e-4 bps/$ = 200 bps per $1M.
        let mut trades = Vec::new();
        let mut px = 100.0;
        let mut xs = Vec::new();
        for i in 0..30 {
            let x = if i % 2 == 0 { 100.0 } else { -100.0 };
            let x = x + i as f64; // break exact symmetry so var > 0 either way
            xs.push(x);
            px *= (x * 2e-4 / 1e4).exp();
            let ts = c - 300_000 + i * 10_000 + 5_000;
            trades.push(t(ts, px, x.abs() / px, x > 0.0));
        }
        // trades from earlier minutes need to be in those minutes; simplest
        // is to reset the state and feed the five minutes properly.
        let mut s = TickState::new();
        for k in 0..5 {
            let m = M0 + 60 * k;
            let lo = m * 1000;
            let hi = lo + 60_000;
            let mine: Vec<TapeTrade> = trades
                .iter()
                .copied()
                .filter(|t| t.ts_ms >= lo && t.ts_ms < hi)
                .collect();
            let f = s.push(m, minute(mine));
            if k == 4 {
                let lam = f[10].expect("impact defined");
                assert!((lam - 200.0).abs() < 1e-6, "lambda {lam}");
            }
        }
        // Flat prices -> y = 0 everywhere -> cov 0 -> Some(0), var > 0.
        let mut s = TickState::new();
        for k in 0..5 {
            let m = M0 + 60 * k;
            let c = (m + 60) * 1000;
            let mine: Vec<TapeTrade> = (0..6)
                .map(|j| {
                    t(
                        c - 60_000 + j * 10_000 + 1,
                        100.0,
                        1.0 + j as f64,
                        j % 2 == 0,
                    )
                })
                .collect();
            let f = s.push(m, minute(mine));
            if k == 4 {
                assert_eq!(f[10], Some(0.0));
            }
        }
        // Fewer than 10 active buckets -> None.
        let mut s = TickState::new();
        for k in 0..5 {
            let m = M0 + 60 * k;
            let c = (m + 60) * 1000;
            let mine = if k == 4 {
                vec![t(c - 1_000, 100.0, 1.0, true)]
            } else {
                vec![]
            };
            let f = s.push(m, minute(mine));
            if k == 4 {
                assert!(f[10].is_none());
            }
        }
        let _ = s;
    }

    #[test]
    fn median_size_is_log1p_of_the_window_median_notional() {
        let mut s = TickState::new();
        for k in 0..4 {
            s.push(M0 + 60 * k, minute(vec![]));
        }
        let m = M0 + 240;
        let c = (m + 60) * 1000;
        let f = s.push(
            m,
            minute(vec![
                t(c - 3_000, 10.0, 1.0, true),
                t(c - 2_000, 10.0, 3.0, true),
                t(c - 1_000, 10.0, 2.0, false),
            ]),
        );
        assert!((f[11].unwrap() - (20.0f64 + 1.0).ln()).abs() < 1e-12);
    }
}
