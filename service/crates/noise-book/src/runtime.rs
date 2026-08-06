//! The book runtime: the engine's G-family session sequence, incremental.
//! Port of bot/runtime.py — same steps, same order, one bar at a time.
//!
//! Safety rails live here because they must be independent of the signal
//! path: EOD flat at the flat_by bar (and retroactively at session
//! rollover for early-close days), and the kill criterion — rolling
//! 60-session book net below the validated unit drawdown halts
//! permanently pending review.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use features_cme::EnrichedBar;

use crate::exec::{
    exit_all, force_flat_price, manage_noise, market_fill, noise_exit_price, Costs, Fill,
    Position, Signal,
};
use crate::scanner::Scanner;
use crate::Sleeve;

pub const KILL_ROLLING_SESSIONS: usize = 60;
pub const KILL_DRAWDOWN_DOLLARS: f64 = -70_500.0;

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[derive(Debug)]
pub struct SleeveState {
    pub cfg: Sleeve,
    pub scanner: Scanner,
    pub position: Option<Position>,
    pub resting: Vec<(Signal, i64)>,
    pub bar_index: i64,
    pub session: Option<NaiveDate>,
    /// The previous completed bar — needed to force-flat retroactively when
    /// a session ends early (holiday half-days never reach the flat_by bar,
    /// and the runtime only learns the session ended when the next begins).
    pub last_bar: Option<EnrichedBar>,
}

/// The operator's control word, already resolved from the canonical
/// document by the caller.
#[derive(Debug, Default, Clone)]
pub struct Control {
    pub halt: bool,
    pub disabled_sleeves: BTreeSet<String>,
    pub instrument: Option<String>,
    pub units: Option<u32>,
}

#[derive(Debug)]
pub struct Book {
    pub sleeves: Vec<SleeveState>,
    pub costs: Costs,
    pub instrument: String,
    pub units: u32,
    pub halted: Option<String>,
    pub disabled: BTreeSet<String>,
    pub fills: Vec<Fill>,
    pub daily_net: BTreeMap<NaiveDate, f64>,
    /// Set when a session boundary was crossed since the last check —
    /// the binary publishes state and re-reads controls on this edge.
    pub session_boundary: bool,
}

impl Book {
    pub fn new(sleeves: Vec<Sleeve>) -> Self {
        Self {
            sleeves: sleeves
                .into_iter()
                .map(|cfg| SleeveState {
                    scanner: Scanner::new(&cfg),
                    cfg,
                    position: None,
                    resting: Vec::new(),
                    bar_index: -1,
                    session: None,
                    last_bar: None,
                })
                .collect(),
            costs: Costs::default(),
            instrument: "MNQ".into(),
            units: 1,
            halted: None,
            disabled: BTreeSet::new(),
            fills: Vec::new(),
            daily_net: BTreeMap::new(),
            session_boundary: false,
        }
    }

    pub fn apply_control(&mut self, c: &Control) {
        if c.halt && self.halted.is_none() {
            self.halted = Some("operator".into());
        }
        self.disabled_sleeves(c);
        if let Some(i) = &c.instrument {
            self.instrument = i.clone();
        }
        if let Some(u) = c.units {
            self.units = u.max(1);
        }
    }

    fn disabled_sleeves(&mut self, c: &Control) {
        self.disabled = c.disabled_sleeves.clone();
    }

    fn kill_check(&mut self) {
        if self.halted.is_some() || self.daily_net.len() < KILL_ROLLING_SESSIONS {
            return;
        }
        let days: Vec<&NaiveDate> = self.daily_net.keys().collect();
        let tail = &days[days.len() - KILL_ROLLING_SESSIONS..];
        let rolling: f64 = tail.iter().map(|d| self.daily_net[*d]).sum();
        if rolling < KILL_DRAWDOWN_DOLLARS {
            self.halted = Some(tail.last().expect("nonempty").to_string());
        }
    }

    fn book_fill(&mut self, fill: Fill) {
        *self.daily_net.entry(fill.session_date).or_insert(0.0) += fill.dollars;
        self.fills.push(fill);
        self.kill_check();
    }

    /// Feed one enriched bar to one sleeve. Bars must arrive in global ts
    /// order; the same bar goes to every sleeve of its frame.
    pub fn on_bar(&mut self, sleeve_idx: usize, bar: &EnrichedBar) {
        if self.halted.is_some() {
            return;
        }
        let costs = self.costs;
        let instrument = self.instrument.clone();
        let units = self.units;
        let halted = self.halted.is_some();
        let disabled;
        let key;
        {
            let st = &self.sleeves[sleeve_idx];
            key = st.cfg.key;
            disabled = self.disabled.contains(key);
        }

        let mut booked: Vec<Fill> = Vec::new();
        let mut rolled = false;

        let st = &mut self.sleeves[sleeve_idx];
        if st.session != Some(bar.session_date) {
            // Session rollover. Normally flat_by already flattened us; on
            // early-close days mirror the engine's session-end force_flat
            // against the session's LAST bar.
            if let Some(pos) = st.position.take() {
                let last = st.last_bar.as_ref().expect("a position implies a prior bar");
                let price = force_flat_price(&pos, last, &costs);
                booked.push(exit_all(
                    key,
                    &instrument,
                    st.session.expect("a position implies a session"),
                    &pos,
                    last,
                    price,
                    "flat_by",
                    &costs,
                ));
            }
            st.session = Some(bar.session_date);
            st.bar_index = -1;
            st.resting.clear();
            st.scanner.reset();
            rolled = true;
        }
        st.bar_index += 1;
        let i = st.bar_index;
        let clock = bar.clock();
        let entry_open = st.cfg.entry_open;
        let flat_by = st.cfg.flat_by;
        let tradeable = clock >= entry_open && clock < flat_by;
        let session = st.session.expect("set above");

        // 1. an open position lives through this bar (engine G-branch)
        if let Some(pos) = st.position.as_mut() {
            let fill = if pos.noise_exit_pending {
                let price = noise_exit_price(pos, bar, &costs);
                Some(exit_all(key, &instrument, session, pos, bar, price, "noise_trail", &costs))
            } else {
                manage_noise(pos, bar);
                if clock >= flat_by {
                    let price = force_flat_price(pos, bar, &costs);
                    Some(exit_all(key, &instrument, session, pos, bar, price, "flat_by", &costs))
                } else {
                    None
                }
            };
            if let Some(f) = fill {
                booked.push(f);
                st.position = None;
            }
        }

        // 2. resting market orders fill at this bar's open (age >= 1)
        if st.position.is_none() && !st.resting.is_empty() && tradeable && !halted && !disabled {
            let resting = std::mem::take(&mut st.resting);
            let mut still = Vec::new();
            for (signal, placed) in resting {
                if st.position.is_some() || i - placed < 1 {
                    still.push((signal, placed));
                    continue;
                }
                let price = market_fill(signal.direction, bar, &costs);
                let mut pos = Position {
                    direction: signal.direction,
                    entry_price: price,
                    entry_ts: bar.ts_utc,
                    entry_index: i as usize,
                    contracts: units,
                    stop_price: signal.stop_price,
                    noise_exit_pending: false,
                };
                manage_noise(&mut pos, bar);
                st.position = Some(pos);
            }
            st.resting = still;
        }

        // 3. expire stale orders
        let cancel_after = st.cfg.cancel_after_bars as i64;
        st.resting.retain(|(_, placed)| i - placed < cancel_after);

        // 4. scan this bar's close
        st.scanner.pos_sign = st.position.as_ref().map(|p| p.sign()).unwrap_or(0.0);
        if let Some(signal) = st.scanner.on_bar(bar) {
            let in_window = tradeable;
            let g_exiting = st
                .position
                .as_ref()
                .map(|p| p.noise_exit_pending)
                .unwrap_or(false);
            let opposite = st
                .position
                .as_ref()
                .map(|p| signal.direction.sign() == -p.sign())
                .unwrap_or(false);
            let blocked = st.position.is_some() && !(g_exiting && opposite);
            if in_window && !blocked {
                st.resting.push((signal, i));
            }
        }
        st.last_bar = Some(bar.clone());

        if rolled {
            self.session_boundary = true;
        }
        for f in booked {
            self.book_fill(f);
        }
    }

    /// The bot-specific `detail` document of the canonical status envelope
    /// (same shape the Python runtime published; the dashboard's futures
    /// view renders it).
    pub fn detail_doc(&self) -> serde_json::Value {
        let days: Vec<&NaiveDate> = self.daily_net.keys().collect();
        let tail_n = days.len().min(KILL_ROLLING_SESSIONS);
        let tail = &days[days.len() - tail_n..];
        let rolling: f64 = tail.iter().map(|d| self.daily_net[*d]).sum();
        let recent: Vec<serde_json::Value> = self
            .daily_net
            .iter()
            .rev()
            .take(30)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|(d, v)| serde_json::json!({"date": d.to_string(), "net": r2(*v)}))
            .collect();
        let net_total: f64 = self.fills.iter().map(|f| f.dollars).sum();
        let sleeves: serde_json::Map<String, serde_json::Value> = self
            .sleeves
            .iter()
            .map(|st| {
                let key = st.cfg.key;
                let n = self.fills.iter().filter(|f| f.sleeve == key).count();
                let net: f64 = self
                    .fills
                    .iter()
                    .filter(|f| f.sleeve == key)
                    .map(|f| f.dollars)
                    .sum();
                let pos = st.position.as_ref().map(|p| {
                    let last = st
                        .last_bar
                        .as_ref()
                        .map(|b| b.close)
                        .unwrap_or(p.entry_price);
                    serde_json::json!({
                        "direction": p.direction,
                        "entry": p.entry_price,
                        "entry_ts": p.entry_ts.to_rfc3339(),
                        "trail": p.stop_price,
                        "last_price": last,
                        "unrealized_dollars": r2((last - p.entry_price) * p.sign()
                            * self.costs.point_value * p.contracts as f64),
                        "bars_held": st.bar_index.saturating_sub(p.entry_index as i64),
                    })
                });
                (
                    key.to_string(),
                    serde_json::json!({
                        "enabled": !self.disabled.contains(key),
                        "in_position": st.position.is_some(),
                        "direction": st.position.as_ref().map(|p| p.direction),
                        "position": pos,
                        "session": st.session.map(|s| s.to_string()),
                        "trades_total": n,
                        "net_total": r2(net),
                    }),
                )
            })
            .collect();
        serde_json::json!({
            "instrument": self.instrument,
            "sizing": {"mode": "fixed", "units": self.units},
            "kill": {
                "rolling_sessions": tail_n,
                "rolling_net": r2(rolling),
                "limit": KILL_DRAWDOWN_DOLLARS,
            },
            "sleeves": sleeves,
            "recent_daily_net": recent,
            "trades_total": self.fills.len(),
            "net_total": r2(net_total),
        })
    }

    /// Per-sleeve (fills, net) — what the parity fixture asserts on.
    pub fn sleeve_totals(&self) -> BTreeMap<&'static str, (usize, f64)> {
        let mut out: BTreeMap<&'static str, (usize, f64)> = BTreeMap::new();
        for st in &self.sleeves {
            out.insert(st.cfg.key, (0, 0.0));
        }
        for f in &self.fills {
            for st in &self.sleeves {
                if st.cfg.key == f.sleeve {
                    let e = out.get_mut(st.cfg.key).expect("inserted");
                    e.0 += 1;
                    e.1 = ((e.1 + f.dollars) * 100.0).round() / 100.0;
                }
            }
        }
        out
    }
}
