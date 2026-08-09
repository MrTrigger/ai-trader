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
    exit_all, force_flat_price, manage_noise, market_fill, noise_exit_price, Costs, Fill, Position,
    Signal,
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
    /// Graceful stop: take nothing new on. What is already open keeps
    /// being managed and closes when the strategy exits it — a halt is not
    /// an exit, but it is not a freeze either.
    pub halt: bool,
    /// Stop AND close: halt, then flatten what is open. The difference
    /// matters at 3am, which is why they are two buttons and not one with
    /// a checkbox.
    pub flatten: bool,
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
        if c.halt {
            if self.halted.is_none() && c.flatten {
                self.halted = Some("operator-stop".into());
            }
            if self.halted.is_none() {
                self.halted = Some("operator".into());
            }
        } else if matches!(
            self.halted.as_deref(),
            Some("operator") | Some("operator-stop")
        ) {
            // An operator halt is operator-resumable. Rail halts (kill
            // criterion, reconcile mismatch, feed stall, refused orders)
            // stay latched: they mean something is WRONG, and clearing them
            // is a restart after a human has looked, not a button.
            self.halted = None;
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
        // NOTE: a halt does not freeze the book — it is a graceful stop.
        //
        // This used to return early, which skipped the whole bar: step 1
        // (managing the open position) never ran, so a halted book held
        // through its own stop-loss, through its noise exit, and through
        // the session-end flat. The operator was told "open positions are
        // left alone" and it was true in the worst sense — a halted bot
        // ignored its own risk exits and carried the position indefinitely.
        //
        // The intended design was already here and unreachable: `halted`
        // below gates step 2, opening a new position, and nothing else. So
        // a halt stops the book taking anything on, and lets what it holds
        // wind down the way the strategy says it should.
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
                let last = st
                    .last_bar
                    .as_ref()
                    .expect("a position implies a prior bar");
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
                Some(exit_all(
                    key,
                    &instrument,
                    session,
                    pos,
                    bar,
                    price,
                    "noise_trail",
                    &costs,
                ))
            } else {
                manage_noise(pos, bar);
                if clock >= flat_by {
                    let price = force_flat_price(pos, bar, &costs);
                    Some(exit_all(
                        key,
                        &instrument,
                        session,
                        pos,
                        bar,
                        price,
                        "flat_by",
                        &costs,
                    ))
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

    /// Everything a dead process needs to continue exactly where it
    /// stopped: machine state per sleeve plus the kill-rail history. Fills
    /// live in the records DB (rehydrated on restore), features re-warm
    /// deterministically from the bar history — neither belongs here.
    pub fn snapshot(&self) -> serde_json::Value {
        let sleeves: serde_json::Map<String, serde_json::Value> = self
            .sleeves
            .iter()
            .map(|st| {
                (
                    st.cfg.key.to_string(),
                    serde_json::json!({
                        "scanner": st.scanner,
                        "position": st.position,
                        "resting": st.resting,
                        "bar_index": st.bar_index,
                        "session": st.session,
                        "last_bar": st.last_bar,
                        "last_ts": st.last_bar.as_ref().map(|b| b.ts_utc),
                    }),
                )
            })
            .collect();
        serde_json::json!({
            "schema": 1,
            "halted": self.halted,
            "instrument": self.instrument,
            "units": self.units,
            "daily_net": self.daily_net.iter()
                .map(|(d, v)| (d.to_string(), *v))
                .collect::<BTreeMap<String, f64>>(),
            "sleeves": sleeves,
        })
    }

    /// Restore machine state from [`Book::snapshot`]'s document. The caller
    /// then feeds only bars strictly AFTER each sleeve's `last_ts` mark —
    /// see [`Book::resume_marks`] — and rehydrates fills from the records
    /// DB so published totals cover the whole book.
    pub fn restore(&mut self, snap: &serde_json::Value) -> Result<(), String> {
        self.halted = snap["halted"].as_str().map(str::to_string);
        if let Some(i) = snap["instrument"].as_str() {
            self.instrument = i.to_string();
        }
        if let Some(u) = snap["units"].as_u64() {
            self.units = u as u32;
        }
        if let Some(dn) = snap["daily_net"].as_object() {
            self.daily_net = dn
                .iter()
                .map(|(d, v)| {
                    NaiveDate::parse_from_str(d, "%Y-%m-%d")
                        .map(|date| (date, v.as_f64().unwrap_or(0.0)))
                        .map_err(|e| format!("bad daily_net date {d:?}: {e}"))
                })
                .collect::<Result<_, _>>()?;
        }
        for st in &mut self.sleeves {
            let s = &snap["sleeves"][st.cfg.key];
            if s.is_null() {
                continue;
            }
            st.scanner = serde_json::from_value(s["scanner"].clone())
                .map_err(|e| format!("{}: scanner: {e}", st.cfg.key))?;
            st.position = serde_json::from_value(s["position"].clone())
                .map_err(|e| format!("{}: position: {e}", st.cfg.key))?;
            st.resting = serde_json::from_value(s["resting"].clone())
                .map_err(|e| format!("{}: resting: {e}", st.cfg.key))?;
            st.bar_index = s["bar_index"].as_i64().unwrap_or(-1);
            st.session = serde_json::from_value(s["session"].clone())
                .map_err(|e| format!("{}: session: {e}", st.cfg.key))?;
            st.last_bar = serde_json::from_value(s["last_bar"].clone())
                .map_err(|e| format!("{}: last_bar: {e}", st.cfg.key))?;
        }
        Ok(())
    }

    /// Per-sleeve last processed ts_utc — the resume feed boundary.
    pub fn resume_marks(&self) -> BTreeMap<&'static str, Option<chrono::DateTime<chrono::Utc>>> {
        self.sleeves
            .iter()
            .map(|st| (st.cfg.key, st.last_bar.as_ref().map(|b| b.ts_utc)))
            .collect()
    }

    /// Rehydrate the trade list (journal rows from the records DB) so
    /// everything a resumed process publishes covers the whole book.
    pub fn rehydrate_fills(&mut self, fills: Vec<Fill>) {
        self.fills = fills;
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

#[cfg(test)]
mod control_tests {
    use super::*;
    use crate::book_sleeves;

    fn book() -> Book {
        Book::new(book_sleeves())
    }
    /// The three canonical control words, as the runtime sees them.
    fn running() -> Control {
        Control::default()
    }
    fn halt() -> Control {
        Control {
            halt: true,
            ..Default::default()
        }
    }
    fn stop() -> Control {
        Control {
            halt: true,
            flatten: true,
            ..Default::default()
        }
    }

    #[test]
    fn every_operator_transition_round_trips() {
        let mut b = book();
        assert!(b.halted.is_none(), "a fresh book trades");

        b.apply_control(&halt());
        assert_eq!(b.halted.as_deref(), Some("operator"));

        b.apply_control(&running());
        assert!(b.halted.is_none(), "Resume must clear an operator halt");

        b.apply_control(&stop());
        assert_eq!(b.halted.as_deref(), Some("operator-stop"));

        // The one the dashboard could not reach until now: after a Stop the
        // only way back is Start, and if this does not clear, the button
        // writes a row and nothing happens.
        b.apply_control(&running());
        assert!(b.halted.is_none(), "Start must clear a stop");
    }

    #[test]
    fn a_rail_halt_is_not_operator_resumable() {
        // Kill criterion, reconcile mismatch, feed stall, refused orders:
        // these mean something is WRONG. Clearing them is a restart after a
        // human has looked, never a button on a web page.
        for reason in ["kill-criterion", "feed-stall", "reconcile-mismatch"] {
            let mut b = book();
            b.halted = Some(reason.into());
            b.apply_control(&running());
            assert_eq!(
                b.halted.as_deref(),
                Some(reason),
                "{reason} must stay latched"
            );
        }
    }

    #[test]
    fn repeating_a_control_is_idempotent() {
        // The loop re-reads the control every bar, so every word is applied
        // over and over. None of them may drift on repetition.
        let mut b = book();
        for _ in 0..5 {
            b.apply_control(&stop());
        }
        assert_eq!(b.halted.as_deref(), Some("operator-stop"));
        for _ in 0..5 {
            b.apply_control(&running());
        }
        assert!(b.halted.is_none());
    }

    #[test]
    fn halting_an_already_stopped_book_keeps_the_stronger_reason() {
        let mut b = book();
        b.apply_control(&stop());
        b.apply_control(&halt());
        assert_eq!(
            b.halted.as_deref(),
            Some("operator-stop"),
            "downgrading to a plain halt would misreport why the book is shut"
        );
    }
}

#[cfg(test)]
mod halt_semantics_tests {
    use super::*;
    use crate::book_sleeves;
    use features_cme::{in_frame, segment, to_exchange_time, Frame, FrameStream, RawBar};

    /// Three months of seeded 5-minute bars — enough to clear the 14-session
    /// noise-band warmup and trade. Same generator as the live-path gate.
    fn bars() -> Vec<RawBar> {
        let t0 = chrono::DateTime::<chrono::Utc>::from_timestamp(1_783_288_800, 0).expect("epoch");
        let mut seed: u64 = 0x5EED_1234_ABCD_0001;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut px = 20_000.0f64;
        (0..24_192)
            .map(|i| {
                let open = px;
                let shock = if rng() < 0.02 { 9.0 } else { 1.0 };
                px = (px + (rng() - 0.5) * 14.0 * shock).max(1_000.0);
                let (hi, lo) = (open.max(px), open.min(px));
                RawBar {
                    ts_utc: t0 + chrono::Duration::minutes(5 * i),
                    open,
                    high: hi + rng() * 5.0,
                    low: lo - rng() * 5.0,
                    close: px,
                    volume: 500.0 + rng() * 3_000.0,
                }
            })
            .collect()
    }

    struct Driver {
        book: Book,
        gx: FrameStream,
        rth: FrameStream,
        frames: Vec<Frame>,
    }

    impl Driver {
        fn new() -> Self {
            let sleeves = book_sleeves();
            let frames = sleeves.iter().map(|s| s.frame).collect();
            Self {
                book: Book::new(sleeves),
                gx: FrameStream::new(Frame::Globex),
                rth: FrameStream::new(Frame::Rth),
                frames,
            }
        }
        fn feed(&mut self, raw: &RawBar) {
            let seg = segment(to_exchange_time(raw.ts_utc));
            let g = self.gx.on_bar(raw);
            let r = in_frame(seg, Frame::Rth).then(|| self.rth.on_bar(raw));
            for (idx, frame) in self.frames.clone().iter().enumerate() {
                match frame {
                    Frame::Globex => self.book.on_bar(idx, &g),
                    Frame::Rth => {
                        if let Some(b) = &r {
                            self.book.on_bar(idx, b);
                        }
                    }
                }
            }
        }
        fn open_positions(&self) -> usize {
            self.book
                .sleeves
                .iter()
                .filter(|s| s.position.is_some())
                .count()
        }
    }

    /// Halt is a GRACEFUL stop: it stops the book taking anything new on,
    /// and lets what it already holds wind down the way the strategy says.
    /// The bug this pins: `on_bar` returned early while halted, so an open
    /// position was never managed again — it sailed through its stop-loss,
    /// its noise exit and the session-end flat, held indefinitely.
    #[test]
    fn a_halted_book_still_closes_what_it_holds() {
        let all = bars();
        let mut d = Driver::new();

        // Run until the book is carrying something.
        let mut i = 0;
        while i < all.len() && d.open_positions() == 0 {
            d.feed(&all[i]);
            i += 1;
        }
        assert!(
            d.open_positions() > 0,
            "never opened a position — test is vacuous"
        );
        let fills_at_halt = d
            .book
            .sleeve_totals()
            .values()
            .map(|(n, _)| *n)
            .sum::<usize>();

        d.book.apply_control(&Control {
            halt: true,
            ..Default::default()
        });

        // A week of further bars. Everything held must wind down.
        let end = (i + 2016).min(all.len());
        for b in &all[i..end] {
            d.feed(b);
        }

        assert_eq!(
            d.open_positions(),
            0,
            "a halted book held its position through a week of bars — exits never ran"
        );
        let fills_after = d
            .book
            .sleeve_totals()
            .values()
            .map(|(n, _)| *n)
            .sum::<usize>();
        assert!(fills_after > fills_at_halt, "the wind-down booked no fills");
    }

    /// The other half of the same contract: graceful, not passive. A halt
    /// must still refuse to open anything new.
    #[test]
    fn a_halted_book_opens_nothing_new() {
        let all = bars();
        let mut d = Driver::new();
        for b in &all[..12_000] {
            d.feed(b);
        }
        d.book.apply_control(&Control {
            halt: true,
            ..Default::default()
        });

        // Let anything already open wind down first.
        let mut i = 12_000;
        while i < all.len() && d.open_positions() > 0 {
            d.feed(&all[i]);
            i += 1;
        }
        assert_eq!(d.open_positions(), 0, "never wound down");

        let fills_flat = d
            .book
            .sleeve_totals()
            .values()
            .map(|(n, _)| *n)
            .sum::<usize>();
        let end = (i + 4032).min(all.len());
        for b in &all[i..end] {
            d.feed(b);
        }
        assert_eq!(d.open_positions(), 0, "a halted book opened a new position");
        assert_eq!(
            d.book
                .sleeve_totals()
                .values()
                .map(|(n, _)| *n)
                .sum::<usize>(),
            fills_flat,
            "a halted book booked a new fill"
        );
    }
}
