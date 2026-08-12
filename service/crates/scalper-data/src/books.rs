//! Turn a raw `L2Book` into the flat snapshot the recorder writes to disk.
//!
//! A one-sided book (no resting bids, or no resting asks) is not a book we
//! can price a cross against — recording it as `bids: []` would let a reader
//! silently treat "we didn't get a full picture" as "there was no liquidity",
//! which is a different fact. So `snapshot_from_book` refuses it instead.

use hyperliquid::L2Book;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookSnapshot {
    /// The client's wall clock (UTC millis) at the moment the fetch
    /// completed - not the venue's book timestamp. Hyperliquid's L2 book
    /// response carries no server-side capture time, so this is a
    /// receive-side stamp only, good for ordering snapshots and rolling day
    /// files, not for measuring venue-side latency.
    pub ts_ms: i64,
    pub coin: String,
    /// (px, sz) pairs, best-first, up to `depth` levels.
    pub bids: Vec<(f64, f64)>,
    /// (px, sz) pairs, best-first, up to `depth` levels.
    pub asks: Vec<(f64, f64)>,
}

/// Parse `book`'s two sides into a `BookSnapshot`, keeping up to `depth`
/// levels of each, best price first (the venue already orders them that way).
///
/// Errors when either side is empty: a one-sided book is data worth
/// refusing, not recording as zeros.
pub fn snapshot_from_book(
    coin: &str,
    ts_ms: i64,
    book: &L2Book,
    depth: usize,
) -> Result<BookSnapshot, String> {
    let bids = side(book, 0, depth);
    let asks = side(book, 1, depth);
    if bids.is_empty() || asks.is_empty() {
        return Err(format!(
            "{coin}: one-sided book (bids={}, asks={})",
            bids.len(),
            asks.len()
        ));
    }
    Ok(BookSnapshot {
        ts_ms,
        coin: coin.to_string(),
        bids,
        asks,
    })
}

fn side(book: &L2Book, i: usize, depth: usize) -> Vec<(f64, f64)> {
    book.levels
        .get(i)
        .map(|levels| {
            levels
                .iter()
                .take(depth)
                .filter_map(|l| Some((l.px.parse().ok()?, l.sz.parse().ok()?)))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperliquid::L2Level;

    fn lvl(px: &str, sz: &str) -> L2Level {
        L2Level {
            px: px.to_string(),
            sz: sz.to_string(),
            n: 1,
        }
    }

    #[test]
    fn a_book_becomes_a_snapshot_best_levels_first() {
        let book = L2Book {
            levels: vec![
                vec![lvl("64000", "1.5"), lvl("63999", "2.0")], // bids
                vec![lvl("64001", "0.7"), lvl("64002", "3.0")], // asks
            ],
        };
        let s = snapshot_from_book("BTC", 1_754_956_800_000, &book, 10).unwrap();
        assert_eq!(s.bids[0], (64000.0, 1.5));
        assert_eq!(s.asks[0], (64001.0, 0.7));
        assert_eq!(s.bids.len(), 2);
    }

    #[test]
    fn depth_caps_the_levels_kept() {
        let book = L2Book {
            levels: vec![
                (0..30).map(|i| lvl(&format!("{}", 64000 - i), "1")).collect(),
                (0..30).map(|i| lvl(&format!("{}", 64001 + i), "1")).collect(),
            ],
        };
        let s = snapshot_from_book("BTC", 0, &book, 10).unwrap();
        assert_eq!(s.bids.len(), 10);
        assert_eq!(s.asks.len(), 10);
    }

    #[test]
    fn a_one_sided_book_is_refused() {
        let book = L2Book {
            levels: vec![vec![], vec![lvl("1", "1")]],
        };
        assert!(snapshot_from_book("X", 0, &book, 10).is_err());
    }
}
