//! Turn recorded book snapshots into the cost a backtest must charge.
//!
//! Two numbers matter: the quoted spread, and what it actually costs to walk
//! the book for a real order size. The spread alone flatters a thin book —
//! `walk_cost_bps` prices the VWAP an order of a given notional would get,
//! and reports `None` rather than a number when the visible depth can't fill
//! it at all. That `None` is itself the finding: a backtest that assumes it
//! can always cross at the spread is assuming liquidity the book doesn't
//! show.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::books::BookSnapshot;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostSummary {
    pub samples: u32,
    pub spread_bps_median: f64,
    pub spread_bps_p75: f64,
    /// Keyed by notional (dollar string, e.g. "5000"). `None` when the
    /// median snapshot couldn't absorb that notional at all - a thin book,
    /// reported as missing rather than papered over with a number.
    pub cross_bps: BTreeMap<String, Option<f64>>,
    pub top_depth_usd_median: f64,
}

/// Depth-walked VWAP cost of `notional_usd`, in bps vs `mid`, crossing one
/// side of the book (port of `hyperliquid/examples/book_capture.rs`'s
/// `walk`, generalized to price either direction).
///
/// `asks_or_bids` is the side being crossed: asks to buy, bids to sell,
/// best-price-first either way. `None` when the visible levels can't absorb
/// the full notional - a thin book, not a zero-cost fill.
pub fn walk_cost_bps(
    asks_or_bids: &[(f64, f64)],
    notional_usd: f64,
    mid: f64,
    is_buy: bool,
) -> Option<f64> {
    if mid <= 0.0 || notional_usd <= 0.0 {
        return None;
    }
    let (mut left, mut spent, mut got) = (notional_usd, 0.0, 0.0);
    for (px, sz) in asks_or_bids {
        let avail = px * sz;
        let take = avail.min(left);
        spent += take;
        got += take / px;
        left -= take;
        if left <= 0.0 {
            break;
        }
    }
    if left > 0.0 || got <= 0.0 {
        return None;
    }
    let vwap = spent / got;
    // Buying costs more than mid (vwap - mid > 0); selling costs by
    // receiving less than mid (mid - vwap > 0). Both read as a positive
    // cost in bps.
    let signed = if is_buy { vwap - mid } else { mid - vwap };
    Some(signed / mid * 1e4)
}

/// One `CostSummary` per coin present in `snapshots`, with `cross_bps`
/// broken out by notional. Pure - no I/O.
///
/// A snapshot's cross cost is the mean of its buy-side (crossing asks) and
/// sell-side (crossing bids) walk cost - a symmetric-cost assumption: with a
/// single snapshot we have no basis to expect one direction's liquidity to
/// be systematically better than the other's, so we average rather than
/// pick a side.
pub fn summarize(snapshots: &[BookSnapshot], notionals: &[f64]) -> BTreeMap<String, CostSummary> {
    let mut by_coin: BTreeMap<&str, Vec<&BookSnapshot>> = BTreeMap::new();
    for snap in snapshots {
        by_coin.entry(snap.coin.as_str()).or_default().push(snap);
    }

    let mut out = BTreeMap::new();
    for (coin, snaps) in by_coin {
        let mut spreads = Vec::with_capacity(snaps.len());
        let mut depths = Vec::with_capacity(snaps.len());
        let mut per_notional: BTreeMap<String, Vec<Option<f64>>> = BTreeMap::new();
        for &notional in notionals {
            per_notional.insert(notional_key(notional), Vec::with_capacity(snaps.len()));
        }

        for snap in &snaps {
            let (Some(&(bid, _)), Some(&(ask, ask_sz))) = (snap.bids.first(), snap.asks.first())
            else {
                continue;
            };
            let mid = (bid + ask) / 2.0;
            if mid <= 0.0 {
                continue;
            }
            spreads.push((ask - bid) / mid * 1e4);
            depths.push(ask * ask_sz);

            for &notional in notionals {
                let buy = walk_cost_bps(&snap.asks, notional, mid, true);
                let sell = walk_cost_bps(&snap.bids, notional, mid, false);
                let combined = match (buy, sell) {
                    (Some(b), Some(s)) => Some((b + s) / 2.0),
                    _ => None,
                };
                per_notional
                    .get_mut(&notional_key(notional))
                    .expect("inserted above")
                    .push(combined);
            }
        }

        let cross_bps = per_notional
            .into_iter()
            .map(|(key, mut vals)| (key, median_opt(&mut vals)))
            .collect();

        out.insert(
            coin.to_string(),
            CostSummary {
                samples: spreads.len() as u32,
                spread_bps_median: median(&spreads),
                spread_bps_p75: percentile(&spreads, 0.75),
                cross_bps,
                top_depth_usd_median: median(&depths),
            },
        );
    }
    out
}

/// Render a notional as its `cross_bps` map key: whole dollar amounts
/// (the only kind this CLI produces) print without a trailing ".0".
fn notional_key(notional: f64) -> String {
    if notional.fract() == 0.0 {
        format!("{}", notional as i64)
    } else {
        notional.to_string()
    }
}

/// Median = midpoint of the sorted values; an even count averages the two
/// middle values. Empty input reports NaN - callers only feed this
/// non-empty per-coin observations.
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Linear-interpolation percentile (`p` in [0, 1]) over the sorted values.
pub(crate) fn percentile(values: &[f64], p: f64) -> f64 {
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

/// Same median convention as `median`, but over `Option<f64>` outcomes where
/// `None` means "this snapshot couldn't absorb the notional at all". `None`
/// sorts as worse than every fillable value, so the result is `None`
/// exactly when the median *snapshot* (by that ordering) was itself
/// unfillable - which is the fact worth surfacing, not averaging away.
fn median_opt(values: &mut [Option<f64>]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| match (a, b) {
        (Some(x), Some(y)) => x.total_cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        match (values[n / 2 - 1], values[n / 2]) {
            (Some(a), Some(b)) => Some((a + b) / 2.0),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(coin: &str, bid: f64, ask: f64, sz: f64) -> BookSnapshot {
        BookSnapshot {
            ts_ms: 0,
            coin: coin.into(),
            bids: vec![(bid, sz), (bid - 1.0, sz * 10.0)],
            asks: vec![(ask, sz), (ask + 1.0, sz * 10.0)],
        }
    }

    #[test]
    fn walking_the_book_prices_the_levels_you_eat() {
        // Buy $128,002 against asks of 1.0@64001 and 3.0@64002: eats all of level
        // one and 1.000015... of level two; VWAP sits between the two prices.
        let asks = vec![(64001.0, 1.0), (64002.0, 3.0)];
        let mid = 64000.5;
        let cost = walk_cost_bps(&asks, 128_002.0, mid, true).unwrap();
        assert!(cost > 0.0 && cost < 2.0, "got {cost} bps");
        assert!(
            walk_cost_bps(&asks, 500_000.0, mid, true).is_none(),
            "book too thin"
        );
    }

    #[test]
    fn summaries_carry_medians_and_thin_books_stay_visible() {
        let snaps: Vec<BookSnapshot> = (0..5).map(|_| snap("BTC", 64000.0, 64001.0, 2.0)).collect();
        let out = summarize(&snaps, &[5_000.0, 50_000_000.0]);
        let btc = &out["BTC"];
        assert_eq!(btc.samples, 5);
        assert!(btc.spread_bps_median > 0.0);
        assert!(btc.cross_bps["5000"].is_some());
        assert!(
            btc.cross_bps["50000000"].is_none(),
            "an unabsorbable notional reads None, not 0"
        );
    }

    #[test]
    fn even_count_median_is_none_when_the_middle_pair_splits_fillable_and_thin() {
        // Four snapshots, same coin: two deep enough to absorb $5,000 on
        // both sides, two too thin to absorb it on either side. Sorted by
        // the Some-before-None convention, the two middle values (index 1
        // and 2) are one Some and one None, so the even-count median must
        // report None - "the middle snapshot was itself unfillable" is the
        // fact worth keeping, not averaged away.
        let deep = snap("BTC", 64000.0, 64001.0, 100.0);
        let thin = snap("BTC", 64000.0, 64001.0, 0.001);
        let snaps = vec![deep.clone(), deep, thin.clone(), thin];
        let out = summarize(&snaps, &[5_000.0]);
        assert!(
            out["BTC"].cross_bps["5000"].is_none(),
            "2 fillable + 2 thin: even-count median must be None, got {:?}",
            out["BTC"].cross_bps["5000"]
        );
    }

    #[test]
    fn even_count_median_is_some_when_the_middle_pair_is_both_fillable() {
        // Three deep snapshots and one thin one: sorted Some-before-None,
        // the two middle values (index 1 and 2) are both Some, so the
        // even-count median averages them into a Some.
        let deep = snap("BTC", 64000.0, 64001.0, 100.0);
        let thin = snap("BTC", 64000.0, 64001.0, 0.001);
        let snaps = vec![deep.clone(), deep.clone(), deep, thin];
        let out = summarize(&snaps, &[5_000.0]);
        assert!(
            out["BTC"].cross_bps["5000"].is_some(),
            "3 fillable + 1 thin: even-count median must be Some, got {:?}",
            out["BTC"].cross_bps["5000"]
        );
    }
}
