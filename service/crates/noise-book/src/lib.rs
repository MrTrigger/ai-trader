//! The NQ four-sleeve noise-area book: decision core in Rust.
//!
//! Ported 1:1 from the validated Python implementation in trading-journal
//! (engine/setup_g.py, engine/run.py `_manage_noise_position`,
//! engine/execution.py, bot/runtime.py) under the operator's language
//! mandate: Rust owns the money path end to end, and the committed parity
//! fixture is the acceptance gate — identical fills or the port does not
//! land. Provenance of every parameter lives in the journal repo's
//! audition ledger (backtest/lab/sleeve_screens.py, rounds 1–14).
//!
//! The four sleeves (adopted 2026-08-06, round 13): breakout + pullback
//! twins on the Globex-open and RTH frames, threshold 0.1, flat by
//! 15:35 ET, each sleeve one independent walk — the book holds up to four
//! contracts concurrently.

pub mod exec;
pub mod runtime;
pub mod scanner;

use chrono::NaiveTime;
use features_cme::Frame;

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).expect("valid time")
}

/// One sleeve's frozen configuration. These values are research artifacts,
/// not tunables: changing any of them requires a new audition in the
/// journal repo's ledger and a regenerated parity fixture.
#[derive(Debug, Clone)]
pub struct Sleeve {
    pub key: &'static str,
    pub frame: Frame,
    /// Breakout must clear the boundary by threshold × sigma × bar-open.
    pub threshold: f64,
    /// Pullback sleeves (0 = breakout mode): arm on a qualifying breakout
    /// close inside [entry_from, entry_until), then enter on this many
    /// consecutive counter-closes while flat. Flat-gating is load-bearing.
    pub pullback_closes: u32,
    /// Scanner windows (exchange time). Breakout mode uses entry_until as
    /// its last signalling close; pullback mode arms inside the window.
    pub entry_from: NaiveTime,
    pub entry_until: NaiveTime,
    /// Runtime entry window: fills may only open inside
    /// [entry_open, flat_by); flat_by force-flattens.
    pub entry_open: NaiveTime,
    pub flat_by: NaiveTime,
    pub cancel_after_bars: usize,
}

/// The book, verbatim from the journal repo's BOOK_SLEEVES + rules-lab.
pub fn book_sleeves() -> Vec<Sleeve> {
    vec![
        Sleeve {
            key: "G-OPEN",
            frame: Frame::Globex,
            threshold: 0.1,
            pullback_closes: 0,
            entry_from: t(0, 0),
            entry_until: t(9, 45),
            entry_open: t(8, 30),
            flat_by: t(15, 35),
            cancel_after_bars: 2,
        },
        Sleeve {
            key: "G-NOISE",
            frame: Frame::Rth,
            threshold: 0.1,
            pullback_closes: 0,
            entry_from: t(0, 0),
            entry_until: t(15, 30),
            entry_open: t(9, 45),
            flat_by: t(15, 35),
            cancel_after_bars: 2,
        },
        Sleeve {
            key: "G-PULL-OPEN",
            frame: Frame::Globex,
            threshold: 0.1,
            pullback_closes: 2,
            entry_from: t(8, 30),
            entry_until: t(9, 45),
            entry_open: t(8, 30),
            flat_by: t(15, 35),
            cancel_after_bars: 2,
        },
        Sleeve {
            key: "G-PULL-RTH",
            frame: Frame::Rth,
            threshold: 0.1,
            pullback_closes: 2,
            entry_from: t(9, 45),
            entry_until: t(15, 30),
            entry_open: t(9, 45),
            flat_by: t(15, 35),
            cancel_after_bars: 2,
        },
    ]
}
