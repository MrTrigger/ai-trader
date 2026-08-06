//! The futures bot: Rust end to end.
//!
//!   replay    stored bars through the full stack; publishes fills/status
//!             to the records DB when DATABASE_URL is set; verifies the
//!             committed parity fixture with --fixture
//!   features  emit enriched bars (JSONL) for training — the SAME
//!             computation the runtime trades on; models select catalog
//!             columns by name with --select
//!
//! Live IB feeding (rust-ibapi) attaches behind the same Book once the
//! venue adapter lands; nothing here changes for it.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::process::ExitCode;

use chrono::NaiveDate;
use features_cme::{in_frame, segment, to_exchange_time, EnrichedBar, Frame, FrameStream, RawBar};
use noise_book::runtime::Book;
use noise_book::book_sleeves;

const USAGE: &str = "\
usage: futures-bot <command>

  replay --bars <jsonl> [--start YYYY-MM-DD] [--end YYYY-MM-DD]
         [--fixture <parity-fixture.json>] [--bot-id ID]
      Replay raw bars through features + the four-sleeve book. With
      DATABASE_URL set, publishes fills, status and the snapshot to the
      records DB under --bot-id (default futures-noise). With --fixture,
      exits non-zero unless per-sleeve fills and net match exactly.

  features --bars <jsonl> --frame rth|globex [--select a,b,c] [--out <jsonl>]
      Emit enriched bars for training. --select restricts to named catalog
      columns (validated; unknown names are an error). One implementation:
      this IS the runtime's feature path.

Bars JSONL rows: {\"ts_utc\":\"2026-06-01T13:30:00Z\",\"open\":..,\"high\":..,
\"low\":..,\"close\":..,\"volume\":..} in ascending ts order.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str);
    let get = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let out = match cmd {
        Some("replay") => replay(&get),
        Some("features") => features_cmd(&get),
        _ => {
            println!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match out {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("futures-bot: {e}");
            ExitCode::FAILURE
        }
    }
}

fn read_bars(path: &str) -> Result<Vec<RawBar>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
    let mut bars = Vec::new();
    for (n, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| format!("{path}:{}: {e}", n + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let bar: RawBar =
            serde_json::from_str(&line).map_err(|e| format!("{path}:{}: {e}", n + 1))?;
        bars.push(bar);
    }
    if bars.is_empty() {
        return Err(format!("{path} holds no bars"));
    }
    Ok(bars)
}

fn parse_date(s: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| format!("bad date {s:?}: {e}"))
}

fn replay(get: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let bars = read_bars(&get("--bars").ok_or("--bars is required")?)?;
    let start = get("--start").map(|s| parse_date(&s)).transpose()?;
    let end = get("--end").map(|s| parse_date(&s)).transpose()?;
    let bot_id = get("--bot-id").unwrap_or_else(|| "futures-noise".into());

    let sleeves = book_sleeves();
    // sleeve index -> frame, in book order (parity with the Python feed's
    // per-ts emission order).
    let frames: Vec<Frame> = sleeves.iter().map(|s| s.frame).collect();
    let mut book = Book::new(sleeves);
    let mut globex = FrameStream::new(Frame::Globex);
    let mut rth = FrameStream::new(Frame::Rth);

    for raw in &bars {
        let seg = segment(to_exchange_time(raw.ts_utc));
        // Enrich once per frame; feature streams consume ALL bars so the
        // replay window never truncates the lookbacks.
        let g_bar: EnrichedBar = globex.on_bar(raw);
        let r_bar: Option<EnrichedBar> =
            in_frame(seg, Frame::Rth).then(|| rth.on_bar(raw));
        let in_window = |b: &EnrichedBar| {
            start.map(|s| b.session_date >= s).unwrap_or(true)
                && end.map(|e| b.session_date <= e).unwrap_or(true)
        };
        for (idx, frame) in frames.iter().enumerate() {
            match frame {
                Frame::Globex => {
                    if in_window(&g_bar) {
                        book.on_bar(idx, &g_bar);
                    }
                }
                Frame::Rth => {
                    if let Some(b) = &r_bar {
                        if in_window(b) {
                            book.on_bar(idx, b);
                        }
                    }
                }
            }
        }
    }

    let totals = book.sleeve_totals();
    println!("{:<14}{:>7}{:>12}", "sleeve", "n", "net");
    for (k, (n, net)) in &totals {
        println!("{k:<14}{n:>7}{net:>+12.0}");
    }
    if let Some(h) = &book.halted {
        println!("KILL CRITERION fired {h}");
    }

    publish(&book, &bot_id)?;

    if let Some(fx) = get("--fixture") {
        return check_fixture(&fx, &totals);
    }
    Ok(())
}

/// Publish fills + the canonical status envelope + the snapshot to the
/// records DB — the same rows, the same envelope, as every bot.
fn publish(book: &Book, bot_id: &str) -> Result<(), String> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("futures-bot {bot_id}: DATABASE_URL not set — results not recorded (dev)");
        return Ok(());
    };
    let rec = records::blocking::Records::connect(&url)
        .map_err(|e| format!("records db unreachable ({e}) — fail closed"))?;
    rec.clear_fills(bot_id).map_err(|e| e.to_string())?;
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
                noise_book::exec::Direction::Long => "long",
                noise_book::exec::Direction::Short => "short",
            }),
            Some(&f.contracts.to_string()),
            Some(&f.exit.to_string()),
            Some(&f.dollars.to_string()),
            Some(f.reason),
            &payload,
        )
        .map_err(|e| e.to_string())?;
    }
    let now = chrono::Utc::now().to_rfc3339();
    let net: f64 = book.fills.iter().map(|f| f.dollars).sum();
    let doc = serde_json::json!({
        "schema": 1,
        "kind": "futures-book",
        "mode": "replay",
        "state": if book.halted.is_some() { "halted" } else { "running" },
        "state_reason": book.halted,
        "headline": {"net": (net * 100.0).round() / 100.0, "fills": book.fills.len(), "unit": "USD"},
        "detail": book.detail_doc(),
    });
    rec.put_status(bot_id, &now, &doc.to_string())
        .map_err(|e| e.to_string())?;
    println!("published {} fills + status to records db as {bot_id}", book.fills.len());
    Ok(())
}

fn check_fixture(
    path: &str,
    totals: &BTreeMap<&'static str, (usize, f64)>,
) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let want: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    let sleeves = want["sleeves"]
        .as_object()
        .ok_or("fixture has no sleeves object")?;
    let mut ok = true;
    for (tag, v) in sleeves {
        let (n, net) = totals
            .iter()
            .find(|(k, _)| *k == tag)
            .map(|(_, t)| *t)
            .unwrap_or((0, 0.0));
        let wn = v["n"].as_u64().unwrap_or(0) as usize;
        let wnet = v["net"].as_f64().unwrap_or(f64::NAN);
        let m = n == wn && (net - wnet).abs() < 0.005;
        if !m {
            ok = false;
        }
        println!(
            "{tag:<14} n {n:>4} vs {wn:>4}   net {net:>+12.2} vs {wnet:>+12.2}   {}",
            if m { "MATCH" } else { "DIFF" }
        );
    }
    if ok {
        println!("PARITY: PASS (Rust port == committed Python fixture)");
        Ok(())
    } else {
        Err("PARITY: FAIL — the port does not reproduce the fixture".into())
    }
}

fn features_cmd(get: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let bars = read_bars(&get("--bars").ok_or("--bars is required")?)?;
    let frame = match get("--frame").as_deref() {
        Some("rth") => Frame::Rth,
        Some("globex") => Frame::Globex,
        other => return Err(format!("--frame must be rth|globex, got {other:?}")),
    };
    let select: Option<Vec<String>> = get("--select")
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
    if let Some(names) = &select {
        features_cme::validate_selection(names)?;
    }
    let mut out: Box<dyn Write> = match get("--out") {
        Some(p) => Box::new(std::fs::File::create(&p).map_err(|e| format!("{p}: {e}"))?),
        None => Box::new(std::io::stdout().lock()),
    };
    let mut stream = FrameStream::new(frame);
    for raw in &bars {
        let seg = segment(to_exchange_time(raw.ts_utc));
        if !in_frame(seg, frame) {
            continue;
        }
        let e = stream.on_bar(raw);
        let row = match &select {
            None => serde_json::to_value(&e).expect("bar serialises"),
            Some(names) => {
                let mut m = serde_json::Map::new();
                m.insert("ts_utc".into(), serde_json::json!(e.ts_utc));
                m.insert("session_date".into(), serde_json::json!(e.session_date));
                for n in names {
                    m.insert(n.clone(), serde_json::json!(features_cme::feature_value(&e, n)));
                }
                serde_json::Value::Object(m)
            }
        };
        writeln!(out, "{row}").map_err(|e| e.to_string())?;
    }
    Ok(())
}
