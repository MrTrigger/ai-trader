//! The ONE implementation of CME-futures session framing and bar features.
//!
//! Operator mandate (2026-08-06): every feature is implemented exactly once,
//! in Rust, and BOTH training and the live runtime link this crate — parity
//! creep between research and production is structurally impossible, not
//! just tested away. Python may orchestrate training, but it consumes
//! frames emitted by this code (`futures-bot features`), never its own.
//!
//! Scoped per instrument type (operator, same day): this crate is CME
//! equity-index futures — the 18:00 Globex roll, RTH segments, and the bar
//! features the futures bots read. Other instrument classes get their own
//! `features-*` crate when a bot needs one; a shared abstraction layer
//! waits until there are two implementations to abstract over.
//!
//! Ported 1:1 from the validated implementation in trading-journal
//! (`backtest/src/backtest/bot/ib_feed.py` FramePrep/LiveFrame, verified
//! there against the batch polars pipeline to 1e-12, and
//! `backtest/src/backtest/sessions.py` for the session logic). The
//! acceptance gate for this port is the committed futures parity fixture:
//! identical fills, or it does not land.
//!
//! Semantics, for the reader who has not seen the lab:
//!
//! * Exchange time is **America/New_York**; a bar's `session_date` is the
//!   CME trade date — ts_et shifted +6h, so a Sunday 18:00 bar belongs to
//!   Monday. Segments: `rth` = [09:30, 16:15), `overnight` = >= 18:00 or
//!   < 09:30, `post` otherwise.
//! * Two frames: `Rth` (rth-segment bars only, 09:30 anchor) and `Globex`
//!   (every bar of the session, 18:00 anchor).
//! * Noise band: slot k of a session compares against sigma_k = the mean
//!   |close/s_open - 1| at slot k over the PRIOR 14 sessions (never
//!   today's); UB = max(s_open, pdc)·(1+sigma), LB = min(s_open, pdc)·
//!   (1−sigma), where pdc is the prior session's last close in the frame.
//! * VWAP is session-scoped over typical price; ATR(14) is the Wilder
//!   recursion with true range reset (plain high−low) on session starts.

use chrono::{DateTime, NaiveDate, NaiveTime, Timelike, Utc};
use chrono_tz::America::New_York;
use chrono_tz::Tz;

pub const ATR_PERIOD: f64 = 14.0;
pub const NOISE_LOOKBACK: usize = 14;

// --- sessions ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Rth,
    Overnight,
    Post,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frame {
    Rth,
    Globex,
}

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).expect("valid time")
}

pub fn rth_open() -> NaiveTime {
    t(9, 30)
}
pub fn rth_close() -> NaiveTime {
    t(16, 15)
}
pub fn globex_open() -> NaiveTime {
    t(18, 0)
}

pub fn to_exchange_time(ts_utc: DateTime<Utc>) -> DateTime<Tz> {
    ts_utc.with_timezone(&New_York)
}

/// CME trade date: exchange time shifted +6h, date taken. Correct across
/// both DST transitions because the shift is nowhere near 02:00.
pub fn session_date(ts_et: DateTime<Tz>) -> NaiveDate {
    (ts_et + chrono::Duration::hours(6)).date_naive()
}

pub fn segment(ts_et: DateTime<Tz>) -> Segment {
    let clock = ts_et.time();
    if clock >= rth_open() && clock < rth_close() {
        Segment::Rth
    } else if clock >= globex_open() || clock < rth_open() {
        Segment::Overnight
    } else {
        Segment::Post
    }
}

pub fn in_frame(seg: Segment, frame: Frame) -> bool {
    match frame {
        Frame::Rth => seg == Segment::Rth,
        Frame::Globex => true,
    }
}

// --- bars -------------------------------------------------------------------

/// A raw completed bar, exactly what a data export or a live feed provides.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RawBar {
    pub ts_utc: DateTime<Utc>,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    #[serde(default)]
    pub volume: f64,
}

/// A bar enriched with everything the decision core reads.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnrichedBar {
    pub ts_utc: DateTime<Utc>,
    /// Exchange-time clock of the bar open (HH:MM:SS as seconds-of-day),
    /// carried precomputed so no consumer ever re-derives it differently.
    pub ts_et_seconds: u32,
    pub session_date: NaiveDate,
    pub slot: usize,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub noise_ub: Option<f64>,
    pub noise_lb: Option<f64>,
    pub noise_sigma: Option<f64>,
    pub vwap: f64,
    pub atr5: Option<f64>,
}

impl EnrichedBar {
    pub fn clock(&self) -> NaiveTime {
        NaiveTime::from_num_seconds_from_midnight_opt(self.ts_et_seconds, 0)
            .expect("clock seconds in range")
    }
}

// --- the incremental frame stream -------------------------------------------

/// Per-slot history of |move| values across sessions, capped at the noise
/// lookback. The mean over a full window is that slot's sigma; fewer than
/// `NOISE_LOOKBACK` values means no band yet — same as the batch
/// shift(1).rolling_mean(14) reading null.
#[derive(Debug, Default, Clone)]
struct SlotHistory {
    values: Vec<f64>, // chronological, len <= NOISE_LOOKBACK
}

impl SlotHistory {
    fn push(&mut self, v: f64) {
        if self.values.len() == NOISE_LOOKBACK {
            self.values.remove(0);
        }
        self.values.push(v);
    }

    fn sigma(&self) -> Option<f64> {
        if self.values.len() < NOISE_LOOKBACK {
            return None;
        }
        // Left-to-right sum in session order — the identical operation order
        // to the Python prep (`sum(tail) / len(tail)`), so the port is
        // bit-equal, not merely close.
        let mut s = 0.0;
        for v in &self.values {
            s += v;
        }
        Some(s / self.values.len() as f64)
    }
}

/// Streams one frame's bars in time order and emits enriched bars.
///
/// This is FramePrep + LiveFrame fused: the per-session prep (sigma table,
/// prior close, ATR seed) is maintained incrementally as sessions complete,
/// which is arithmetic-identical to rebuilding it from history each morning
/// — and it is the same code path live bars flow through.
#[derive(Debug)]
pub struct FrameStream {
    pub frame: Frame,
    slots: Vec<SlotHistory>,
    /// |move| values of the CURRENT session per slot — folded into `slots`
    /// only when the session completes, so today never sees itself.
    session_moves: Vec<f64>,
    /// Sigma table frozen at session start (reads would otherwise shift as
    /// `slots` grows mid-history — they must not; the batch table is fixed
    /// per session).
    sigma_today: Vec<Option<f64>>,
    session: Option<NaiveDate>,
    s_open: Option<f64>,
    pdc: Option<f64>,
    last_close: Option<f64>,
    slot: usize,
    cum_pv: f64,
    cum_v: f64,
    atr: Option<f64>,
    prev_close_for_tr: Option<f64>,
    first_bar_of_session: bool,
}

impl FrameStream {
    pub fn new(frame: Frame) -> Self {
        Self {
            frame,
            slots: Vec::new(),
            session_moves: Vec::new(),
            sigma_today: Vec::new(),
            session: None,
            s_open: None,
            pdc: None,
            last_close: None,
            slot: 0,
            cum_pv: 0.0,
            cum_v: 0.0,
            atr: None,
            prev_close_for_tr: None,
            first_bar_of_session: true,
        }
    }

    fn roll_session(&mut self, new_session: NaiveDate) {
        // Fold the finished session's moves into the slot histories.
        for (k, mv) in self.session_moves.drain(..).enumerate() {
            if self.slots.len() <= k {
                self.slots.push(SlotHistory::default());
            }
            self.slots[k].push(mv);
        }
        self.pdc = self.last_close;
        self.session = Some(new_session);
        self.s_open = None;
        self.slot = 0;
        self.cum_pv = 0.0;
        self.cum_v = 0.0;
        self.first_bar_of_session = true;
        self.sigma_today = self.slots.iter().map(SlotHistory::sigma).collect();
    }

    /// Feed the next bar of this frame (caller filters by segment via
    /// [`in_frame`]); bars must arrive in ts order.
    pub fn on_bar(&mut self, bar: &RawBar) -> EnrichedBar {
        let ts_et = to_exchange_time(bar.ts_utc);
        let session = session_date(ts_et);
        if self.session != Some(session) {
            self.roll_session(session);
        }

        let slot = self.slot;
        self.slot += 1;
        let s_open = *self.s_open.get_or_insert(bar.open);

        let (h, l, c) = (bar.high, bar.low, bar.close);
        let tp = (h + l + c) / 3.0;
        let v = bar.volume.max(0.0);
        self.cum_pv += tp * v;
        self.cum_v += v;
        let vwap = if self.cum_v > 0.0 {
            self.cum_pv / self.cum_v
        } else {
            c
        };

        // Wilder ATR with session-reset true range: the first bar of a
        // session uses plain high-low (no gap vs the other frame's close).
        let tr = if self.first_bar_of_session || self.prev_close_for_tr.is_none() {
            h - l
        } else {
            let prev = self.prev_close_for_tr.expect("checked");
            (h - l).max((h - prev).abs()).max((l - prev).abs())
        };
        self.atr = Some(match self.atr {
            None => tr,
            Some(seed) => seed + (tr - seed) / ATR_PERIOD,
        });
        self.prev_close_for_tr = Some(c);
        self.first_bar_of_session = false;

        self.session_moves.push((c / s_open - 1.0).abs());

        let sigma = self.sigma_today.get(slot).copied().flatten();
        let (ub, lb) = match (sigma, self.pdc) {
            (Some(sig), Some(pdc)) => (
                Some(s_open.max(pdc) * (1.0 + sig)),
                Some(s_open.min(pdc) * (1.0 - sig)),
            ),
            _ => (None, None),
        };
        self.last_close = Some(c);

        EnrichedBar {
            ts_utc: bar.ts_utc,
            ts_et_seconds: ts_et.time().num_seconds_from_midnight(),
            session_date: session,
            slot,
            open: bar.open,
            high: h,
            low: l,
            close: c,
            volume: bar.volume,
            noise_ub: ub,
            noise_lb: lb,
            noise_sigma: sigma,
            vwap,
            atr5: self.atr,
        }
    }
}

// --- the feature catalog ----------------------------------------------------

/// Stable feature names. A MODEL DECLARES THE SUBSET IT USES by these names
/// (in its manifest/config); training exports and runtime inference both
/// resolve the names through [`feature_value`] — the same computation, the
/// same accessor, so a model can never train on one definition and run on
/// another. Names are append-only: renaming one orphans every model that
/// declared it.
pub const CATALOG: &[&str] = &[
    "open",
    "high",
    "low",
    "close",
    "volume",
    "vwap",
    "atr14",
    "noise_sigma",
    "noise_ub",
    "noise_lb",
    "slot",
];

/// Resolve one catalog feature on an enriched bar. `None` either means the
/// feature is warming up (e.g. no band before 14 sessions) or the name is
/// not in the catalog — validate selections with [`validate_selection`]
/// first so the two cannot be confused.
pub fn feature_value(bar: &EnrichedBar, name: &str) -> Option<f64> {
    match name {
        "open" => Some(bar.open),
        "high" => Some(bar.high),
        "low" => Some(bar.low),
        "close" => Some(bar.close),
        "volume" => Some(bar.volume),
        "vwap" => Some(bar.vwap),
        "atr14" => bar.atr5,
        "noise_sigma" => bar.noise_sigma,
        "noise_ub" => bar.noise_ub,
        "noise_lb" => bar.noise_lb,
        "slot" => Some(bar.slot as f64),
        _ => None,
    }
}

/// A model's declared feature subset, checked against the catalog. Unknown
/// names are an error at load time, never a silent null at inference time.
pub fn validate_selection(names: &[String]) -> Result<(), String> {
    let unknown: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| !CATALOG.contains(n))
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unknown features {:?} — the catalog is {:?}",
            unknown, CATALOG
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn bar(ts: &str, o: f64, h: f64, l: f64, c: f64) -> RawBar {
        RawBar {
            ts_utc: Utc.datetime_from_str(ts, "%Y-%m-%d %H:%M").unwrap(),
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 100.0,
        }
    }

    #[test]
    fn sunday_evening_belongs_to_monday() {
        // 2026-07-05 is a Sunday; 18:00 ET = 22:00 UTC (EDT).
        let ts = Utc
            .datetime_from_str("2026-07-05 22:00", "%Y-%m-%d %H:%M")
            .unwrap();
        let et = to_exchange_time(ts);
        assert_eq!(
            session_date(et),
            NaiveDate::from_ymd_opt(2026, 7, 6).unwrap()
        );
        assert_eq!(segment(et), Segment::Overnight);
    }

    #[test]
    fn rth_boundaries_are_half_open() {
        // 09:30 ET is rth, 16:15 ET is not (EDT: 13:30 / 20:15 UTC).
        let open = to_exchange_time(
            Utc.datetime_from_str("2026-07-06 13:30", "%Y-%m-%d %H:%M")
                .unwrap(),
        );
        let close = to_exchange_time(
            Utc.datetime_from_str("2026-07-06 20:15", "%Y-%m-%d %H:%M")
                .unwrap(),
        );
        assert_eq!(segment(open), Segment::Rth);
        assert_eq!(segment(close), Segment::Post);
    }

    #[test]
    fn no_band_before_lookback_sessions_and_today_never_sees_itself() {
        let mut fs = FrameStream::new(Frame::Rth);
        // 14 sessions of one bar each -> the 15th session has a sigma.
        for d in 1..=15u32 {
            let ts = format!("2026-06-{d:02} 14:00");
            let e = fs.on_bar(&bar(&ts, 100.0, 101.0, 99.0, 100.0 + d as f64 * 0.1));
            if d <= 14 {
                assert!(e.noise_sigma.is_none(), "day {d} must have no band yet");
            } else {
                assert!(e.noise_sigma.is_some(), "day 15 sees 14 prior sessions");
            }
        }
    }

    #[test]
    fn vwap_is_session_scoped() {
        let mut fs = FrameStream::new(Frame::Globex);
        fs.on_bar(&bar("2026-06-01 14:00", 100.0, 100.0, 100.0, 100.0));
        let e = fs.on_bar(&bar("2026-06-02 14:00", 200.0, 200.0, 200.0, 200.0));
        assert_eq!(
            e.vwap, 200.0,
            "new session must not blend yesterday's prints"
        );
    }
}
