//! Execution primitives: fills, positions, exits. Port of
//! engine/execution.py — only what family G uses; the generic intrabar
//! stop/target machinery deliberately does not exist here, because family
//! G's trail is a close-through rule and a touch-based stop would exit
//! trades the validated strategy holds.

use chrono::{DateTime, Utc};
use features_cme::EnrichedBar;

#[derive(Debug, Clone, Copy)]
pub struct Costs {
    pub tick_size: f64,
    /// Dollars per index point (NQ: 20 — the book's validated accounting
    /// unit, independent of which contract actually trades).
    pub point_value: f64,
    /// Round-turn, per contract.
    pub commission: f64,
    pub slippage_ticks: f64,
}

impl Default for Costs {
    fn default() -> Self {
        Self {
            tick_size: 0.25,
            point_value: 20.0,
            commission: 4.50,
            slippage_ticks: 1.0,
        }
    }
}

impl Costs {
    pub fn slip(&self) -> f64 {
        self.slippage_ticks * self.tick_size
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Long,
    Short,
}

impl Direction {
    pub fn sign(self) -> f64 {
        match self {
            Direction::Long => 1.0,
            Direction::Short => -1.0,
        }
    }
}

/// A scanner emission: decide on this close, act on the next bar's open.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Signal {
    pub direction: Direction,
    /// The close that signalled (reference only; entry is market).
    pub trigger_price: f64,
    /// The live trail at signal time — the protective level.
    pub stop_price: f64,
    pub signal_ts: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub direction: Direction,
    pub entry_price: f64,
    pub entry_ts: DateTime<Utc>,
    pub entry_index: usize,
    pub contracts: u32,
    /// The live trail (max(VWAP, UB) long / min(VWAP, LB) short), advanced
    /// every bar so state always shows the current risk.
    pub stop_price: f64,
    /// The exit was DECIDED on the last close and EXECUTES at the next
    /// bar's open — the validated simulator's next-open action.
    pub noise_exit_pending: bool,
}

impl Position {
    pub fn sign(&self) -> f64 {
        self.direction.sign()
    }
}

/// A completed round trip, the unit the journal and the fills table record.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Fill {
    pub sleeve: String,
    pub session_date: chrono::NaiveDate,
    pub direction: Direction,
    pub instrument: String,
    pub contracts: u32,
    pub entry: f64,
    pub exit: f64,
    pub exit_ts: DateTime<Utc>,
    pub points: f64,
    pub dollars: f64,
    pub reason: &'static str,
}

/// Market entry: fills at the bar's open plus adverse slippage. Filling at
/// the signal bar's own close would be lookahead.
pub fn market_fill(direction: Direction, bar: &EnrichedBar, costs: &Costs) -> f64 {
    bar.open + costs.slip() * direction.sign()
}

/// Family-G per-bar management: advance the trail to max(VWAP, UB) long /
/// min(VWAP, LB) short, and if this bar CLOSED through it, mark the exit
/// pending for the next open. Never exits intrabar — the trail is a
/// close-through rule.
pub fn manage_noise(pos: &mut Position, bar: &EnrichedBar) {
    let (Some(ub), Some(lb)) = (bar.noise_ub, bar.noise_lb) else {
        return;
    };
    let vwap = bar.vwap;
    if pos.sign() > 0.0 {
        let trail = vwap.max(ub);
        pos.stop_price = trail;
        if bar.close < trail {
            pos.noise_exit_pending = true;
        }
    } else {
        let trail = vwap.min(lb);
        pos.stop_price = trail;
        if bar.close > trail {
            pos.noise_exit_pending = true;
        }
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Book a full exit at `price` on `bar` and return the completed fill.
pub fn exit_all(
    sleeve: &str,
    instrument: &str,
    session: chrono::NaiveDate,
    pos: &Position,
    bar: &EnrichedBar,
    price: f64,
    reason: &'static str,
    costs: &Costs,
) -> Fill {
    let pts = (price - pos.entry_price) * pos.sign();
    let n = pos.contracts.max(1);
    let dollars = (pts * costs.point_value - costs.commission) * n as f64;
    Fill {
        sleeve: sleeve.to_string(),
        session_date: session,
        direction: pos.direction,
        instrument: instrument.to_string(),
        contracts: n,
        entry: pos.entry_price,
        exit: price,
        exit_ts: bar.ts_utc,
        points: round2(pts),
        dollars: round2(dollars),
        reason,
    }
}

/// "Flat by 15:35, always": market out at the bar's close with slippage.
pub fn force_flat_price(pos: &Position, bar: &EnrichedBar, costs: &Costs) -> f64 {
    bar.close - pos.sign() * costs.slip()
}

/// The pending noise-trail exit executes at this bar's open with slippage.
pub fn noise_exit_price(pos: &Position, bar: &EnrichedBar, costs: &Costs) -> f64 {
    bar.open - costs.slip() * pos.sign()
}
