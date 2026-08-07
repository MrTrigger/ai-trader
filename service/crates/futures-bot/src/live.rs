//! The live loop: IB 5-second bars -> completed 5-minute bars -> the same
//! FrameStream + Book that replay parity proved. Decision logic lives
//! entirely in noise-book; this module is wiring and rails.
//!
//! Modes:
//!   shadow  no orders, ever — the Book simulates fills from live bars and
//!           publishes them (the fill-model calibration month).
//!   live    the Book's position transitions are mirrored with market
//!           orders through the ARMED IbVenue; broker-vs-model
//!           reconciliation runs at session boundaries and any mismatch
//!           flattens and halts (never auto-corrected).
//!
//! Rails, independent of the signal path:
//!   * controls (records DB) re-read every completed bar; halt requests
//!     stop new entries and, in live mode, flatten.
//!   * feed-stall watchdog: no 5s bar for 90s -> flatten (live) and halt.
//!   * snapshot to the records DB after every completed bar batch — the
//!     crash-recovery contract replay proved.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use features_cme::{in_frame, segment, to_exchange_time, EnrichedBar, Frame, FrameStream, RawBar};
use noise_book::book_sleeves;
use noise_book::exec::Direction;
use noise_book::runtime::{Book, Control};
use venue::VenueAdapter;

pub const STALL_SECONDS: u64 = 90;

/// Whether CME equity-index futures SHOULD be printing bars right now:
/// Sun 18:00 ET through Fri 17:00 ET, minus the daily 17:00-18:00 break.
/// Holidays are deliberately unmodelled — a holiday reads as a stall,
/// halts, and a human resumes; that failure direction costs opportunity,
/// not money (same philosophy as venue::calendar::CmeGlobex).
pub fn market_should_be_open(ts_utc: DateTime<Utc>) -> bool {
    use chrono::{Datelike, Timelike, Weekday};
    let et = features_cme::to_exchange_time(ts_utc);
    let (wd, hour) = (et.weekday(), et.hour());
    match wd {
        Weekday::Sat => false,
        Weekday::Sun => hour >= 18,
        Weekday::Fri => hour < 17,
        _ => hour != 17,
    }
}

/// Resolve the canonical control document (schema 1) — or the legacy
/// dialect — into the runtime's Control. Unknown states read as halted.
pub fn control_from_payload(payload: &serde_json::Value) -> Control {
    let (halt, overrides) = if payload.get("schema").is_some() {
        (
            payload["state"].as_str() != Some("running"),
            payload.get("overrides").cloned().unwrap_or_default(),
        )
    } else {
        (
            payload["halt"].as_bool().unwrap_or(false),
            payload.clone(),
        )
    };
    let disabled = overrides["sleeves"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(_, v)| v.as_bool() == Some(false))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    Control {
        halt,
        disabled_sleeves: disabled,
        instrument: overrides["instrument"].as_str().map(str::to_string),
        units: overrides["sizing"]["fixed_units"]
            .as_u64()
            .map(|u| u as u32),
    }
}

/// Fold 5-second bars into 5-minute buckets; returns the COMPLETED bucket
/// when a boundary is crossed. Bucket timestamps are the bar OPEN, the
/// store's convention.
#[derive(Debug, Default)]
pub struct Aggregate5s {
    current: Option<RawBar>,
}

impl Aggregate5s {
    pub fn on_bar(
        &mut self,
        ts: DateTime<Utc>,
        o: f64,
        h: f64,
        l: f64,
        c: f64,
        v: f64,
    ) -> Option<RawBar> {
        let secs = ts.timestamp();
        let bucket_start = secs - secs.rem_euclid(300);
        let bucket = DateTime::<Utc>::from_timestamp(bucket_start, 0).expect("valid ts");
        match &mut self.current {
            Some(cur) if cur.ts_utc == bucket => {
                cur.high = cur.high.max(h);
                cur.low = cur.low.min(l);
                cur.close = c;
                cur.volume += v;
                None
            }
            _ => {
                let done = self.current.take();
                self.current = Some(RawBar {
                    ts_utc: bucket,
                    open: o,
                    high: h,
                    low: l,
                    close: c,
                    volume: v,
                });
                done
            }
        }
    }
}

/// One completed 5-minute bar through the whole stack. Returns the fills
/// booked by this bar (already recorded in the Book) so the live mirror
/// can act on them.
pub struct BarOutcome {
    pub session_rolled: bool,
    /// (sleeve, direction, delta_contracts): +units on entry, -units on
    /// exit, from comparing positions before and after the bar.
    pub transitions: Vec<(String, Direction, i64)>,
}

pub fn drive_bar(
    book: &mut Book,
    globex: &mut FrameStream,
    rth: &mut FrameStream,
    frames: &[Frame],
    raw: &RawBar,
) -> BarOutcome {
    let before: Vec<Option<(Direction, u32)>> = book
        .sleeves
        .iter()
        .map(|s| s.position.as_ref().map(|p| (p.direction, p.contracts)))
        .collect();

    let seg = segment(to_exchange_time(raw.ts_utc));
    let g_bar: EnrichedBar = globex.on_bar(raw);
    let r_bar: Option<EnrichedBar> = in_frame(seg, Frame::Rth).then(|| rth.on_bar(raw));
    book.session_boundary = false;
    for (idx, frame) in frames.iter().enumerate() {
        match frame {
            Frame::Globex => book.on_bar(idx, &g_bar),
            Frame::Rth => {
                if let Some(b) = &r_bar {
                    book.on_bar(idx, b);
                }
            }
        }
    }

    let mut transitions = Vec::new();
    for (i, st) in book.sleeves.iter().enumerate() {
        let after = st.position.as_ref().map(|p| (p.direction, p.contracts));
        match (before[i], after) {
            (None, Some((d, n))) => transitions.push((st.cfg.key.to_string(), d, n as i64)),
            (Some((d, n)), None) => transitions.push((st.cfg.key.to_string(), d, -(n as i64))),
            _ => {}
        }
    }
    BarOutcome {
        session_rolled: book.session_boundary,
        transitions,
    }
}

/// The model's expected net contracts, for broker-vs-model reconciliation.
pub fn model_net_contracts(book: &Book) -> i64 {
    book.sleeves
        .iter()
        .filter_map(|s| s.position.as_ref())
        .map(|p| p.sign() as i64 * p.contracts as i64)
        .sum()
}

/// Warm the feature streams (and nothing else) over historical bars.
pub fn warm_features(
    bars: &[RawBar],
    globex: &mut FrameStream,
    rth: &mut FrameStream,
    until: Option<DateTime<Utc>>,
) -> Option<NaiveDate> {
    let mut last_session = None;
    for raw in bars {
        if let Some(u) = until {
            if raw.ts_utc > u {
                break;
            }
        }
        let seg = segment(to_exchange_time(raw.ts_utc));
        let e = globex.on_bar(raw);
        last_session = Some(e.session_date);
        if in_frame(seg, Frame::Rth) {
            rth.on_bar(raw);
        }
    }
    last_session
}

/// Mirror a Book transition with a market order through the venue. The
/// client_order_id carries bot, sleeve, direction and bar ts — idempotent
/// per decision, attributable in the ledger and on the broker statement.
pub async fn mirror_transition(
    adapter: &dyn VenueAdapter,
    bot_id: &str,
    symbol: &str,
    sleeve: &str,
    direction: Direction,
    delta: i64,
    ts: DateTime<Utc>,
) -> Result<String, venue::VenueError> {
    use rust_decimal::Decimal;
    // Entry in the position's direction; exit is the opposite side.
    let opening = delta > 0;
    let side = match (direction, opening) {
        (Direction::Long, true) | (Direction::Short, false) => venue::Side::Buy,
        _ => venue::Side::Sell,
    };
    let qty = Decimal::from(delta.unsigned_abs());
    let id = format!(
        "{bot_id}-{sleeve}-{}-{}-{}",
        if opening { "O" } else { "C" },
        match side {
            venue::Side::Buy => "B",
            venue::Side::Sell => "S",
        },
        ts.timestamp()
    );
    let ack = adapter
        .place_order(&venue::OrderRequest {
            client_order_id: id.clone(),
            asset: venue::AssetId::from(symbol),
            side,
            qty,
            order_type: venue::OrderType::Market,
            limit_price: None,
            reason: if opening { venue::OrderReason::Entry } else { venue::OrderReason::Exit },
        })
        .await?;
    Ok(ack.venue_order_id)
}

/// Snapshot + status + fills to the records DB — one publish per bar batch.
pub fn publish_live(
    rec: &records::blocking::Records,
    book: &Book,
    bot_id: &str,
    mode: &str,
    note: Option<&str>,
) -> Result<(), String> {
    for f in &book.fills {
        let key = format!(
            "{}|{}|{}|{}|{}|{}",
            f.sleeve, f.session_date, f.exit_ts, f.entry, f.exit, f.dollars
        );
        let payload = serde_json::to_string(f).expect("fill serialises");
        rec.record_fill(
            bot_id,
            &key,
            &f.exit_ts.to_rfc3339(),
            Some(&f.instrument),
            Some(&f.sleeve),
            Some(match f.direction {
                Direction::Long => "long",
                Direction::Short => "short",
            }),
            Some(&f.contracts.to_string()),
            Some(&f.exit.to_string()),
            Some(&f.dollars.to_string()),
            Some(&f.reason),
            &payload,
        )
        .map_err(|e| e.to_string())?;
    }
    let now = chrono::Utc::now().to_rfc3339();
    let net: f64 = book.fills.iter().map(|f| f.dollars).sum();
    let mut detail = book.detail_doc();
    if let Some(n) = note {
        detail["note"] = serde_json::json!(n);
    }
    let doc = serde_json::json!({
        "schema": 1,
        "kind": "futures-book",
        "mode": mode,
        "state": if book.halted.is_some() { "halted" } else { "running" },
        "state_reason": book.halted,
        "headline": {
            "net": (net * 100.0).round() / 100.0,
            "fills": book.fills.len(),
            "unit": "USD",
        },
        "detail": detail,
    });
    rec.put_status(bot_id, &now, &doc.to_string())
        .map_err(|e| e.to_string())?;
    rec.put_snapshot(bot_id, &now, &book.snapshot().to_string())
        .map_err(|e| e.to_string())
}

/// Shared setup for the live loop: sleeves, frames, warm streams, optional
/// snapshot restore + fill rehydration.
pub struct LiveSetup {
    pub book: Book,
    pub globex: FrameStream,
    pub rth: FrameStream,
    pub frames: Vec<Frame>,
    pub marks: BTreeMap<&'static str, Option<DateTime<Utc>>>,
}

pub fn prepare(
    warmup: &[RawBar],
    rec: &records::blocking::Records,
    bot_id: &str,
) -> Result<LiveSetup, String> {
    let sleeves = book_sleeves();
    let frames: Vec<Frame> = sleeves.iter().map(|s| s.frame).collect();
    let mut book = Book::new(sleeves);
    let mut globex = FrameStream::new(Frame::Globex);
    let mut rth = FrameStream::new(Frame::Rth);
    warm_features(warmup, &mut globex, &mut rth, None);

    let mut marks: BTreeMap<&'static str, Option<DateTime<Utc>>> = Default::default();
    if let Some(snap) = rec.get_snapshot(bot_id).map_err(|e| e.to_string())? {
        let snap: serde_json::Value =
            serde_json::from_str(&snap).map_err(|e| format!("snapshot: {e}"))?;
        book.restore(&snap)?;
        let mut fills: Vec<noise_book::exec::Fill> = rec
            .recent_fills(bot_id, 100_000)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter_map(|t| serde_json::from_str::<noise_book::exec::Fill>(&t).ok())
            .collect();
        fills.reverse();
        let n = fills.len();
        book.rehydrate_fills(fills);
        marks = book.resume_marks();
        let open = book.sleeves.iter().filter(|s| s.position.is_some()).count();
        eprintln!("restored snapshot: {open} open position(s), {n} prior trades rehydrated");
    }
    Ok(LiveSetup {
        book,
        globex,
        rth,
        frames,
        marks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregation_completes_buckets_on_the_boundary() {
        let mut agg = Aggregate5s::default();
        let t0 = DateTime::<Utc>::from_timestamp(1_754_000_100, 0).unwrap(); // :35 past a bucket
        assert!(agg.on_bar(t0, 1.0, 2.0, 0.5, 1.5, 10.0).is_none());
        assert!(agg
            .on_bar(t0 + chrono::Duration::seconds(5), 1.5, 3.0, 1.0, 2.0, 5.0)
            .is_none());
        // Next bucket: the previous one completes, merged.
        let done = agg
            .on_bar(t0 + chrono::Duration::seconds(300), 2.0, 2.5, 1.8, 2.2, 7.0)
            .expect("bucket completed");
        assert_eq!(done.high, 3.0);
        assert_eq!(done.low, 0.5);
        assert_eq!(done.close, 2.0);
        assert_eq!(done.volume, 15.0);
        assert_eq!(done.ts_utc.timestamp() % 300, 0, "stamped at bucket open");
    }

    #[test]
    fn control_resolution_fails_closed_on_unknown_states() {
        let c = control_from_payload(&serde_json::json!({
            "schema": 1, "state": "wedged", "reason": "?"
        }));
        assert!(c.halt);
        let c = control_from_payload(&serde_json::json!({
            "schema": 1, "state": "running",
            "overrides": {"sleeves": {"G-NOISE": false}, "sizing": {"fixed_units": 2}}
        }));
        assert!(!c.halt);
        assert!(c.disabled_sleeves.contains("G-NOISE"));
        assert_eq!(c.units, Some(2));
        // legacy dialect
        let c = control_from_payload(&serde_json::json!({"halt": true}));
        assert!(c.halt);
    }
}
