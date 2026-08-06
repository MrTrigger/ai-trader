//! The family-G scanner: breakout mode and the flat-gated pullback machine.
//! Port of engine/setup_g.py — the comments there carry the provenance;
//! here only the mechanics, faithfully.

use chrono::NaiveTime;
use features_cme::EnrichedBar;

use crate::exec::{Direction, Signal};
use crate::Sleeve;

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).expect("valid time")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Scanner {
    threshold: f64,
    pullback_closes: u32,
    entry_from: NaiveTime,
    entry_until: NaiveTime,
    /// Set by the runtime each bar: the walk's current position sign.
    /// The pullback machine is FROZEN while a position is open — the
    /// load-bearing cooldown.
    pub pos_sign: f64,
    armed: i8,
    counter: u32,
    prev_close: Option<f64>,
}

impl Scanner {
    pub fn new(sleeve: &Sleeve) -> Self {
        Self {
            threshold: sleeve.threshold,
            pullback_closes: sleeve.pullback_closes,
            entry_from: sleeve.entry_from,
            entry_until: sleeve.entry_until,
            pos_sign: 0.0,
            armed: 0,
            counter: 0,
            prev_close: None,
        }
    }

    /// Session boundary: breakout mode is stateless; the pullback machine
    /// resets per session.
    pub fn reset(&mut self) {
        self.pos_sign = 0.0;
        self.armed = 0;
        self.counter = 0;
        self.prev_close = None;
    }

    pub fn on_bar(&mut self, bar: &EnrichedBar) -> Option<Signal> {
        let (Some(ub), Some(lb), Some(sigma)) = (bar.noise_ub, bar.noise_lb, bar.noise_sigma)
        else {
            if self.pullback_closes > 0 {
                // sim parity: feature-less bars still advance prev_close
                self.prev_close = Some(bar.close);
            }
            return None;
        };
        match bar.atr5 {
            Some(a) if a > 0.0 => {}
            _ => {
                if self.pullback_closes > 0 {
                    self.prev_close = Some(bar.close);
                }
                return None;
            }
        }

        let close = bar.close;
        // The threshold edge scales by the BAR OPEN (the anchor price).
        let edge = self.threshold * sigma * bar.open;
        let vwap = bar.vwap;

        if self.pullback_closes > 0 {
            let clock = bar.clock();
            // sim parity: the session's afternoon tail is dead for the
            // machine; evening bars of the globex frame keep running.
            if clock >= t(15, 35) && clock < t(17, 30) {
                return None;
            }
            return self.pullback(bar, close, ub, lb, vwap, edge);
        }

        if bar.clock() >= self.entry_until {
            return None;
        }
        let (direction, trail) = if close > ub + edge {
            (Direction::Long, vwap.max(ub))
        } else if close < lb - edge {
            (Direction::Short, vwap.min(lb))
        } else {
            return None;
        };
        emit(bar, direction, close, trail)
    }

    /// Flat-gated pullback machine (lab/noise_pullback.py verbatim): frozen
    /// while the walk holds a position; arm on a breakout close inside the
    /// window; enter on n consecutive counter-closes.
    fn pullback(
        &mut self,
        bar: &EnrichedBar,
        close: f64,
        ub: f64,
        lb: f64,
        vwap: f64,
        edge: f64,
    ) -> Option<Signal> {
        let prev = self.prev_close;
        self.prev_close = Some(close);
        if self.pos_sign != 0.0 {
            return None;
        }
        if self.armed > 0 && close < vwap.max(ub) {
            self.armed = 0;
        } else if self.armed < 0 && close > vwap.min(lb) {
            self.armed = 0;
        }
        let clock = bar.clock();
        if self.armed == 0 {
            if clock >= self.entry_from && clock < self.entry_until {
                if close > ub + edge {
                    self.armed = 1;
                    self.counter = 0;
                } else if close < lb - edge {
                    self.armed = -1;
                    self.counter = 0;
                }
            }
            return None;
        }
        let prev = prev?;
        let is_counter = if self.armed > 0 {
            close < prev
        } else {
            close > prev
        };
        self.counter = if is_counter { self.counter + 1 } else { 0 };
        if self.counter < self.pullback_closes {
            return None;
        }
        let direction = if self.armed > 0 {
            Direction::Long
        } else {
            Direction::Short
        };
        self.armed = 0;
        self.counter = 0;
        let trail = if direction == Direction::Long {
            vwap.max(ub)
        } else {
            vwap.min(lb)
        };
        emit(bar, direction, close, trail)
    }
}

fn emit(bar: &EnrichedBar, direction: Direction, close: f64, trail: f64) -> Option<Signal> {
    // A zero-risk signal (close exactly on the trail) is not tradeable.
    if (close - trail).abs() <= 0.0 {
        return None;
    }
    Some(Signal {
        direction,
        trigger_price: close,
        stop_price: trail,
        signal_ts: bar.ts_utc,
    })
}
