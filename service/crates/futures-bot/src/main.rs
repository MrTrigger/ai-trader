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

mod live;

use ibapi::prelude::StreamExt as _;
use ibapi::prelude::SubscriptionItemStreamExt as _;

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

  ib-check [--live]
      Gateway readiness probe: connection, server version, account, the
      resolved front month, balances, positions, and a 15s market-data
      test. Run this the day the market-data subscription activates.

  run --bars <warmup jsonl> [--live] [--bot-id ID]
      The live loop: IB 5s bars -> 5min -> the SAME stack replay parity
      proved. Default is SHADOW (never armed, simulated fills, published
      to the records DB). --live arms per the .env flags and mirrors the
      book's transitions with market orders; broker-vs-model mismatch at
      a session boundary flattens and halts. Requires DATABASE_URL and a
      registered, ENABLED bot — fail closed.

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
        Some("ib-check") => block_on_async(ib_check(&get)),
        Some("run") => block_on_async(run_live(&get)),
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
    // Mid-session crash simulation: stop feeding at this EXCHANGE-time
    // stamp (YYYY-MM-DDTHH:MM); positions stay open in the snapshot.
    let freeze_at: Option<chrono::NaiveDateTime> = get("--freeze-at")
        .map(|s| {
            chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M")
                .map_err(|e| format!("bad --freeze-at {s:?}: {e}"))
        })
        .transpose()?;
    let resume = get("--resume").is_some() || std::env::args().any(|a| a == "--resume");

    let sleeves = book_sleeves();
    // sleeve index -> frame, in book order (parity with the Python feed's
    // per-ts emission order).
    let frames: Vec<Frame> = sleeves.iter().map(|s| s.frame).collect();
    let mut book = Book::new(sleeves);

    let rec = match std::env::var("DATABASE_URL") {
        Ok(url) => Some(
            records::blocking::Records::connect(&url)
                .map_err(|e| format!("records db unreachable ({e}) — fail closed"))?,
        ),
        Err(_) => None,
    };

    let mut marks: std::collections::BTreeMap<&'static str, Option<chrono::DateTime<chrono::Utc>>> =
        Default::default();
    if resume {
        let rec = rec
            .as_ref()
            .ok_or("--resume needs DATABASE_URL: the snapshot lives in the records db")?;
        let snap = rec
            .get_snapshot(&bot_id)
            .map_err(|e| e.to_string())?
            .ok_or("--resume: no snapshot row found")?;
        let snap: serde_json::Value =
            serde_json::from_str(&snap).map_err(|e| format!("snapshot: {e}"))?;
        book.restore(&snap)?;
        // The snapshot holds machine state; the fills table holds the actual
        // prior trades. Rehydrate so everything published covers the book.
        let mut fills: Vec<noise_book::exec::Fill> = rec
            .recent_fills(&bot_id, 100_000)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter_map(|t| serde_json::from_str::<noise_book::exec::Fill>(&t).ok())
            .collect();
        fills.reverse(); // recent_fills is newest-first
        let n = fills.len();
        book.rehydrate_fills(fills);
        marks = book.resume_marks();
        let open: usize = book.sleeves.iter().filter(|s| s.position.is_some()).count();
        println!("resumed from snapshot: {open} open position(s), {n} prior trades rehydrated");
    } else if let Some(rec) = rec.as_ref() {
        rec.clear_fills(&bot_id).map_err(|e| e.to_string())?;
    }

    let mut globex = FrameStream::new(Frame::Globex);
    let mut rth = FrameStream::new(Frame::Rth);

    for raw in &bars {
        let seg = segment(to_exchange_time(raw.ts_utc));
        // Enrich once per frame; feature streams consume ALL bars so the
        // replay window never truncates the lookbacks.
        let g_bar: EnrichedBar = globex.on_bar(raw);
        let r_bar: Option<EnrichedBar> =
            in_frame(seg, Frame::Rth).then(|| rth.on_bar(raw));
        if let Some(fz) = freeze_at {
            let et_naive = to_exchange_time(raw.ts_utc).naive_local();
            if et_naive > fz {
                continue;
            }
        }
        let in_window = |b: &EnrichedBar| {
            start.map(|s| b.session_date >= s).unwrap_or(true)
                && end.map(|e| b.session_date <= e).unwrap_or(true)
        };
        for (idx, frame) in frames.iter().enumerate() {
            let key = book.sleeves[idx].cfg.key;
            let after_mark = |b: &EnrichedBar| {
                marks
                    .get(key)
                    .and_then(|m| *m)
                    .map(|m| b.ts_utc > m)
                    .unwrap_or(true)
            };
            match frame {
                Frame::Globex => {
                    if in_window(&g_bar) && after_mark(&g_bar) {
                        book.on_bar(idx, &g_bar);
                    }
                }
                Frame::Rth => {
                    if let Some(b) = &r_bar {
                        if in_window(b) && after_mark(b) {
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

    match rec.as_ref() {
        Some(rec) => publish(rec, &book, &bot_id, "replay")?,
        None => eprintln!(
            "futures-bot {bot_id}: DATABASE_URL not set — results not recorded (dev)"
        ),
    }

    if let Some(fx) = get("--fixture") {
        return check_fixture(&fx, &totals);
    }
    Ok(())
}

/// Publish fills + the canonical status envelope + the snapshot to the
/// records DB — the same rows, the same envelope, as every bot. Fill
/// inserts are idempotent on their content key, so republishing after a
/// resume converges instead of duplicating.
fn publish(
    rec: &records::blocking::Records,
    book: &Book,
    bot_id: &str,
    mode: &str,
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
                noise_book::exec::Direction::Long => "long",
                noise_book::exec::Direction::Short => "short",
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
    let doc = serde_json::json!({
        "schema": 1,
        "kind": "futures-book",
        "mode": mode,
        "state": if book.halted.is_some() { "halted" } else { "running" },
        "state_reason": book.halted,
        "headline": {"net": (net * 100.0).round() / 100.0, "fills": book.fills.len(), "unit": "USD"},
        "detail": book.detail_doc(),
    });
    rec.put_status(bot_id, &now, &doc.to_string())
        .map_err(|e| e.to_string())?;
    rec.put_snapshot(bot_id, &now, &book.snapshot().to_string())
        .map_err(|e| e.to_string())?;
    println!(
        "published {} fills + status + snapshot to records db as {bot_id}",
        book.fills.len()
    );
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

fn block_on_async(
    fut: impl std::future::Future<Output = Result<(), String>>,
) -> Result<(), String> {
    tokio::runtime::Runtime::new()
        .map_err(|e| e.to_string())?
        .block_on(fut)
}

async fn ib_check(get: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let live = get("--live").is_some() || std::env::args().any(|a| a == "--live");
    let cfg = ib::IbConfig::from_env(live)?;
    println!("connecting to {}:{} ...", cfg.host, cfg.port);
    let venue = ib::IbVenue::connect(cfg).await.map_err(|e| e.to_string())?;
    println!("connected: server v{}", venue.server_version());
    println!("venue:     {}", venue.describe());
    match venue::VenueAdapter::get_balances(&venue).await {
        Ok(b) => {
            for bal in b {
                println!("balance:   {} {} (available {})", bal.total, bal.currency, bal.available);
            }
        }
        Err(e) => println!("balance:   UNAVAILABLE ({e})"),
    }
    match venue::VenueAdapter::get_positions(&venue).await {
        Ok(p) if p.is_empty() => println!("positions: flat"),
        Ok(p) => {
            for pos in p {
                println!("position:  {} {} @ {}", pos.qty, pos.asset, pos.avg_price);
            }
        }
        Err(e) => println!("positions: UNAVAILABLE ({e})"),
    }
    println!("market data: subscribing to 5s bars for 15s ...");
    let sub = venue
        .client()
        .realtime_bars(venue.contract())
        .subscribe()
        .await
        .map_err(|e| format!("realtime bars refused: {e}"))?;
    let mut stream = sub.filter_data();
    let mut n = 0u32;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while let Some(bar) = stream.next().await {
            if bar.is_ok() {
                n += 1;
            }
        }
    })
    .await;
    if n > 0 {
        println!("market data: LIVE ({n} bars in 15s) — shadow can start");
        Ok(())
    } else {
        Err("market data: NOT streaming (subscription inactive or off-hours)".into())
    }
}


async fn run_live(get: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let live = get("--live").is_some() || std::env::args().any(|a| a == "--live");
    let mode = if live { "live" } else { "shadow" };
    let bot_id = get("--bot-id").unwrap_or_else(|| "futures-noise".into());
    let warmup = read_bars(&get("--bars").ok_or("--bars is required (feature warmup)")?)?;

    // Fail closed twice: the records DB must be reachable, and the bot must
    // be registered AND enabled — running is a deliberate act.
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| "run requires DATABASE_URL (records + controls live there)")?;
    let reg = records::Registry::connect(&url)
        .await
        .map_err(|e| format!("records db unreachable ({e}) — fail closed"))?;
    reg.require_enabled(&bot_id).await.map_err(|e| e.to_string())?;
    let rec = records::blocking::Records::connect(&url).map_err(|e| e.to_string())?;

    let mut cfg = ib::IbConfig::from_env(live)?;
    if !live {
        // Shadow NEVER arms, whatever the env says.
        cfg.allow_orders = false;
    }
    let symbol = cfg.symbol.clone();
    let venue_conn = ib::IbVenue::connect(cfg).await.map_err(|e| e.to_string())?;
    eprintln!("{}", venue_conn.describe());
    if live && !venue_conn.is_armed() {
        return Err(
            "--live but the adapter is not armed (IB_ALLOW_ORDERS / IB_ALLOW_LIVE) —              refusing a live loop that could only fail at the first order"
                .into(),
        );
    }

    let live::LiveSetup {
        mut book,
        mut globex,
        mut rth,
        frames,
        marks,
    } = live::prepare(&warmup, &rec, &bot_id)?;
    if marks.values().flatten().next().is_some() {
        eprintln!(
            "NOTE: resumed marks exist; live bars between the crash and now are not              backfilled yet — the session's earlier bars are only in the snapshot's state."
        );
    }

    let sub = venue_conn
        .client()
        .realtime_bars(venue_conn.contract())
        .subscribe()
        .await
        .map_err(|e| format!("realtime bars refused: {e}"))?;
    let mut stream = sub.filter_data();
    let mut agg = live::Aggregate5s::default();
    eprintln!("run loop started ({mode}); ctrl-c to stop");

    loop {
        let next = tokio::time::timeout(
            std::time::Duration::from_secs(live::STALL_SECONDS),
            stream.next(),
        )
        .await;
        let bar5s = match next {
            Err(_) => {
                // Feed stall: the watchdog rail. Flatten (live), halt, tell.
                if live {
                    let _ = venue_conn.flatten_all(&bot_id).await;
                }
                book.halted = Some("feed-stall".into());
                live::publish_live(&rec, &book, &bot_id, mode, Some("feed stall — halted"))?;
                return Err(format!("no 5s bar for {}s — flattened (live) and halted", live::STALL_SECONDS));
            }
            Ok(None) => {
                book.halted = Some("feed-closed".into());
                live::publish_live(&rec, &book, &bot_id, mode, Some("feed closed — halted"))?;
                return Err("the bar stream ended — halted".into());
            }
            Ok(Some(Err(e))) => {
                eprintln!("bar stream error: {e}; continuing");
                continue;
            }
            Ok(Some(Ok(b))) => b,
        };

        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(bar5s.date.unix_timestamp(), 0)
            .ok_or("bar timestamp out of range")?;
        let Some(done) = agg.on_bar(ts, bar5s.open, bar5s.high, bar5s.low, bar5s.close, bar5s.volume)
        else {
            continue;
        };

        // Controls, fresh every completed bar. Halt stops entries now and,
        // in live mode, flattens.
        let control = rec
            .current_control(&bot_id)
            .map_err(|e| e.to_string())?
            .map(|row| {
                serde_json::from_str(&row.payload)
                    .map(|v: serde_json::Value| live::control_from_payload(&v))
                    .unwrap_or(noise_book::runtime::Control {
                        halt: true,
                        ..Default::default()
                    })
            })
            .unwrap_or_default();
        let was_halted = book.halted.is_some();
        book.apply_control(&control);
        if book.halted.is_some() && !was_halted && live {
            let _ = venue_conn.flatten_all(&bot_id).await;
        }

        let outcome = live::drive_bar(&mut book, &mut globex, &mut rth, &frames, &done);

        if live {
            for (sleeve, direction, delta) in &outcome.transitions {
                if let Err(e) = live::mirror_transition(
                    &venue_conn,
                    &bot_id,
                    &symbol,
                    sleeve,
                    *direction,
                    *delta,
                    done.ts_utc,
                )
                .await
                {
                    // An order the venue refused is a halt, not a retry loop.
                    let _ = venue_conn.flatten_all(&bot_id).await;
                    book.halted = Some(format!("order-refused: {e}"));
                    break;
                }
            }
            if outcome.session_rolled {
                // Broker-vs-model reconciliation at the boundary. Mismatch is
                // never auto-corrected: flatten and halt for a human.
                let venue_net: i64 = venue::VenueAdapter::get_positions(&venue_conn)
                    .await
                    .map_err(|e| e.to_string())?
                    .iter()
                    .filter(|p| p.asset.to_string() == symbol)
                    .map(|p| {
                        let q: f64 = p.qty.try_into().unwrap_or(0.0);
                        q as i64
                    })
                    .sum();
                let model_net = live::model_net_contracts(&book);
                if venue_net != model_net {
                    let _ = venue_conn.flatten_all(&bot_id).await;
                    book.halted = Some(format!(
                        "reconcile-mismatch: broker {venue_net} vs model {model_net}"
                    ));
                }
            }
        }

        live::publish_live(&rec, &book, &bot_id, mode, None)?;
        if book.halted.is_some() {
            eprintln!("halted: {:?} — loop stays up publishing state", book.halted);
        }
    }
}
