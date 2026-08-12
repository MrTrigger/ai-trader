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
use noise_book::book_sleeves;
use noise_book::runtime::Book;

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

  backfill --bars <jsonl> [--days N]
      Fetch recent 5-minute bars from IB historical data and append the
      ones newer than the file's last bar. Closes the gap between the
      lab's stored history and now, so today's noise bands are computed
      from actual recent sessions. With no file — or one whose last bar
      predates the window, which could only be extended across a gap —
      it writes the window instead: IB is the seed as well as the tail.

  rithmic-check [--live]
      Rithmic readiness probe: connect + login all plants, positions read,
      history bars. Needs the RITHMIC_* credentials (.env section 8).

  register [--bot-id ID] [--state-dir DIR] [--launch CMD] [--account ID]
           [--ib-account NUMBER] [--enable]
      Write this bot's identity into the records DB: the IB account it
      trades, the trade binding, the bots row, and the launch command the
      api spawns for Start. Idempotent — safe on every pod start. --launch
      is deployment-specific (a repo path on a laptop, /usr/local/bin in
      the image), so it comes from whoever deploys rather than from a
      migration. Enabling stays a separate act unless --enable is given.

  clear-halt --by NAME --reason TEXT [--bot-id ID]
      Clear a LATCHED rail halt (kill criterion, reconcile mismatch,
      refused order, feed stall). Rail halts live in the snapshot and
      survive every restart on purpose, so this is the only way out — and
      it refuses unless the broker's position agrees with the stored
      book. Run it with the bot STOPPED; a live process republishes its
      snapshot and would write the halt straight back.

  run --bars <warmup jsonl> [--live] [--bot-id ID] [--venue ib|rithmic]
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
        Some("rithmic-check") => block_on_async(rithmic_check(&get)),
        Some("backfill") => block_on_async(backfill(&get)),
        Some("register") => block_on_async(register(&args, &get)),
        Some("clear-halt") => block_on_async(clear_halt(&get)),
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
        let r_bar: Option<EnrichedBar> = in_frame(seg, Frame::Rth).then(|| rth.on_bar(raw));
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
        None => {
            eprintln!("futures-bot {bot_id}: DATABASE_URL not set — results not recorded (dev)")
        }
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

fn check_fixture(path: &str, totals: &BTreeMap<&'static str, (usize, f64)>) -> Result<(), String> {
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
    let select: Option<Vec<String>> =
        get("--select").map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
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
                    m.insert(
                        n.clone(),
                        serde_json::json!(features_cme::feature_value(&e, n)),
                    );
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
    let cfg = match ib::IbConfig::from_env(live) {
        Ok(c) => c,
        Err(e) if e.contains("account not configured") => {
            // The probe's job is discovery: connect anyway, report what the
            // Gateway holds, and continue with it if it is unambiguous.
            let host = std::env::var("IB_GATEWAY_HOST")
                .ok()
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| "127.0.0.1".into());
            let port: u16 = std::env::var("IB_GATEWAY_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(4002);
            let addr = format!("{host}:{port}");
            let client = ibapi::Client::connect(&addr, 8)
                .await
                .map_err(|e| format!("cannot connect to IB Gateway at {addr}: {e}"))?;
            let accounts = client.managed_accounts().await.map_err(|e| e.to_string())?;
            println!("gateway holds account(s): {accounts:?}");
            let [only] = accounts.as_slice() else {
                let account_key = if live {
                    "IB_LIVE_ACCOUNT"
                } else {
                    "IB_PAPER_ACCOUNT"
                };
                return Err(format!(
                    "{e}. Set {account_key} to one of {accounts:?} in .env."
                ));
            };
            let account_key = if live {
                "IB_LIVE_ACCOUNT"
            } else {
                "IB_PAPER_ACCOUNT"
            };
            println!("using {only} for this probe — set {account_key} in .env to keep it");
            drop(client);
            std::env::set_var(account_key, only);
            ib::IbConfig::from_env(live)?
        }
        Err(e) => return Err(e),
    };
    println!("connecting to {}:{} ...", cfg.host, cfg.port);
    let venue = ib::IbVenue::connect(cfg).await.map_err(|e| e.to_string())?;
    println!("connected: server v{}", venue.server_version());
    println!("venue:     {}", venue.describe());
    match venue::VenueAdapter::get_balances(&venue).await {
        Ok(b) => {
            for bal in b {
                println!(
                    "balance:   {} {} (available {})",
                    bal.total, bal.currency, bal.available
                );
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

/// Put this bot's identity in the database, idempotently.
///
/// Registration used to be hand-written SQL against the laptop's Postgres,
/// which meant a second environment had a schema and no rows: the fleet page
/// listed one bot, and the futures bot could not be started because nothing
/// knew it existed. This is that SQL, in the binary that owns the contract, so
/// a fresh database is one command from being deployable.
async fn register(args: &[String], get: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let flag = |name: &str| args.iter().any(|a| a == name);
    let bot_id = get("--bot-id").unwrap_or_else(|| "futures-noise".into());
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| "register requires DATABASE_URL (the identity registry)")?;
    let reg = records::Registry::connect(&url)
        .await
        .map_err(|e| format!("records db unreachable ({e}) — fail closed"))?;
    // The schema comes first: on a fresh database there is nothing to insert
    // into, and a deployment that has to remember to migrate separately is a
    // deployment that will forget.
    reg.migrate().await.map_err(|e| e.to_string())?;

    // The account, then the binding, then the bot. The IB account NUMBER is
    // credentials-adjacent and lives in the environment; only the slug and the
    // credential reference are recorded.
    let account = get("--account").unwrap_or_else(|| "ib-paper".into());
    let ib_account = get("--ib-account")
        .or_else(|| std::env::var("IB_PAPER_ACCOUNT").ok())
        .filter(|a| !a.is_empty());
    let kind = if account.ends_with("-live") {
        "live"
    } else {
        "paper"
    };
    let credential_ref = if kind == "live" {
        "IB_LIVE"
    } else {
        "IB_PAPER"
    };
    reg.register_account(
        &account,
        "ib",
        kind,
        credential_ref,
        "USD",
        ib_account
            .as_deref()
            .map(|a| format!("IB {kind} account {a}"))
            .as_deref(),
    )
    .await
    .map_err(|e| e.to_string())?;
    println!("account {account} ({kind}, credentials {credential_ref})");

    reg.register_bot(
        &bot_id,
        "NQ noise-area four-sleeve book",
        "bar_close(CME,5m)",
        "futures",
        "service/crates/noise-book + features-cme (Rust, gated on \
         bots/futures/parity-fixture.json)",
        get("--state-dir").as_deref(),
        get("--launch").as_deref(),
    )
    .await
    .map_err(|e| e.to_string())?;
    reg.set_trade_binding(&bot_id, &account)
        .await
        .map_err(|e| e.to_string())?;
    println!("bot {bot_id} registered, trading {account}");

    if flag("--enable") {
        reg.set_enabled(&bot_id, true)
            .await
            .map_err(|e| e.to_string())?;
        println!("{bot_id} enabled");
    } else {
        let bot = reg.bot(&bot_id).await.map_err(|e| e.to_string())?;
        if !bot.map(|b| b.enabled).unwrap_or(false) {
            println!("{bot_id} is DISABLED — enable it deliberately (--enable)");
        }
    }
    Ok(())
}

/// Clear a LATCHED rail halt, but only against the broker's agreement.
///
/// Rail halts — kill criterion, reconcile mismatch, refused order, feed stall —
/// latch on purpose: they mean something is wrong and an operator must look.
/// They also live in the snapshot, so `restore` reads them back and the bot
/// comes up halted forever. There was no way out at all: Start clears only
/// operator halts, by design, so a book that halted itself stayed down through
/// every restart with nothing to do about it.
///
/// This is that way out, and it refuses to be a rubber stamp: it reads the
/// broker and the stored book, and clears only if they agree. Papering over a
/// real divergence is exactly what the latch exists to prevent.
///
/// Run it with the bot STOPPED. A live process republishes its snapshot every
/// cycle and would write the halt straight back.
async fn clear_halt(get: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let bot_id = get("--bot-id").unwrap_or_else(|| "futures-noise".into());
    let by = get("--by").ok_or("--by is required: clearing a rail halt is signed")?;
    let reason =
        get("--reason").ok_or("--reason is required: say what you found before clearing")?;
    let url = std::env::var("DATABASE_URL").map_err(|_| "clear-halt requires DATABASE_URL")?;
    let rec = records::blocking::Records::connect(&url).map_err(|e| e.to_string())?;

    let snap_text = rec
        .get_snapshot(&bot_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no snapshot for {bot_id} — nothing to clear"))?;
    let mut snap: serde_json::Value =
        serde_json::from_str(&snap_text).map_err(|e| format!("snapshot: {e}"))?;
    let Some(halted) = snap["halted"].as_str().map(str::to_string) else {
        println!("{bot_id} is not halted — nothing to clear");
        return Ok(());
    };

    // What the stored book thinks it holds.
    let sleeves = book_sleeves();
    let mut book = Book::new(sleeves);
    book.restore(&snap)?;
    let model_net = live::model_net_contracts(&book);

    // What the broker actually holds. Unreadable is NOT agreement.
    let cfg = ib::IbConfig::from_env(false)?;
    let symbol = cfg.symbol.clone();
    let venue = ib::IbVenue::connect(cfg).await.map_err(|e| e.to_string())?;
    let venue_net: i64 = venue::VenueAdapter::get_positions(&venue)
        .await
        .map_err(|e| format!("cannot read broker positions ({e}) — refusing to clear blind"))?
        .iter()
        .filter(|p| p.asset.to_string() == symbol)
        .map(|p| {
            let q: f64 = p.qty.try_into().unwrap_or(0.0);
            q as i64
        })
        .sum();
    if venue_net != model_net {
        return Err(format!(
            "broker {venue_net} vs model {model_net} in {symbol} — they still disagree, so the \
             halt stands. Reconcile the account first."
        ));
    }

    snap["halted"] = serde_json::Value::Null;
    rec.put_snapshot(&bot_id, &chrono::Utc::now().to_rfc3339(), &snap.to_string())
        .map_err(|e| e.to_string())?;
    let line = format!(
        "rail halt cleared by {by}: {reason} (was: {halted}; broker and model both {model_net})"
    );
    let _ = rec.log_line(&bot_id, "warn", &line);
    println!("{line}");
    println!("Start the bot when you are ready; it comes up unhalted.");
    Ok(())
}

/// The broker's net contracts in `symbol`, as the reconcile counts them.
async fn venue_net_contracts(trading: &Trading, symbol: &str) -> Result<i64, String> {
    Ok(trading
        .adapter()
        .get_positions()
        .await
        .map_err(|e| e.to_string())?
        .iter()
        .filter(|p| p.asset.to_string() == symbol)
        .map(|p| {
            let q: f64 = p.qty.try_into().unwrap_or(0.0);
            q as i64
        })
        .sum())
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
    reg.require_enabled(&bot_id)
        .await
        .map_err(|e| e.to_string())?;
    let rec = records::blocking::Records::connect(&url).map_err(|e| e.to_string())?;

    // Which venue trades this bot is DATA: the registry's trade binding
    // decides (--venue overrides for dev). The operator switches brokers by
    // updating the binding, not by editing code.
    let bound = reg
        .bindings(&bot_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|b| b.scope == "trade");
    let venue_id = get("--venue")
        .or_else(|| bound.as_ref().map(|b| b.venue_id.clone()))
        .unwrap_or_else(|| "ib".into());
    // The ADAPTER is chosen by protocol, never by the broker's name: AMP and
    // Ironbeam are different companies reached the same way, and a bot that
    // matched on "amp" would need a code change to add the second one.
    let protocol = bound
        .as_ref()
        .filter(|b| b.venue_id == venue_id)
        .map(|b| b.protocol.clone())
        .unwrap_or_else(|| match venue_id.as_str() {
            // --venue given without a binding (dev): the only names we can
            // resolve without the registry are the ones we ship adapters for.
            "amp" | "rithmic" => "rithmic".into(),
            other => other.to_string(),
        });
    // Which MONEY is a separate axis from which broker, and both are the
    // binding's business: the bound account's kind (paper|live) selects the
    // credential block. `--live` remains what it always was — whether the
    // loop places orders at all (vs shadow) — and never picks the account.
    let live_account = bound
        .as_ref()
        .map(|b| b.account_kind == "live")
        .unwrap_or(false);
    let yes = |k: &str| {
        matches!(
            std::env::var(k).unwrap_or_default().to_lowercase().as_str(),
            "yes" | "true" | "1"
        )
    };
    // The account/env block follows the arming intent, exactly like the
    // venue registry: the LIVE block only when --live AND the operator set
    // the venue's ALLOW_LIVE; otherwise paper/demo — which is what "--live
    // on a paper account" means.
    let allow_live_key = match protocol.as_str() {
        "rithmic" => "RITHMIC_ALLOW_LIVE",
        _ => "IB_ALLOW_LIVE",
    };
    if live_account && !yes(allow_live_key) {
        return Err(format!(
            "the trade binding points at a LIVE account, but {allow_live_key} is not set. \
             Real money needs both: the binding (what the operator chose) and the env flag \
             (what the deployment permits)."
        ));
    }
    let trading = match protocol.as_str() {
        "ib" => {
            let mut cfg = ib::IbConfig::from_env(live_account)?;
            if !live {
                cfg.allow_orders = false; // shadow NEVER arms
            }
            Trading::Ib(std::sync::Arc::new(
                ib::IbVenue::connect(cfg).await.map_err(|e| e.to_string())?,
            ))
        }
        "rithmic" => {
            let mut cfg = rithmic::RithmicCfg::from_env(live_account)?;
            if !live {
                cfg.allow_orders = false;
            }
            Trading::Rithmic(std::sync::Arc::new(
                rithmic::RithmicVenue::connect(cfg)
                    .await
                    .map_err(|e| e.to_string())?,
            ))
        }
        other => {
            return Err(format!(
                "no adapter speaks {other:?} (the {venue_id:?} binding). The futures bot \
                 speaks the ib and rithmic protocols."
            ))
        }
    };
    let symbol = trading.symbol_root();
    eprintln!("broker {venue_id} via {protocol}: {}", trading.describe());
    if live && !trading.is_armed() {
        return Err(
            "--live but the adapter is not armed (the venue's ALLOW_ORDERS / ALLOW_LIVE \
             flags) — refusing a live loop that could only fail at the first order"
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

    // The feed rides its OWN Gateway connection (client id +1): a feed
    // hiccup must never share fate with the order path. Realtime 5s bars
    // are tried first; if nothing arrives (no live CME subscription — the
    // situation on this account today), the producer falls back to polling
    // IB historical 5-minute bars, which this account DOES have. The bot
    // decides on completed bars either way; polling adds seconds of
    // latency to a decision that executes at the next bar open.
    // The feed's health is shared, not inferred: the producer knows why it
    // is quiet, and the loop publishes what it knows.
    // The operator's controls arrive by push, not poll: a trigger on
    // control_events NOTIFYs, this listener wakes the loop within
    // milliseconds of the row committing. If the listener cannot be set up
    // (or its connection later dies) the channel closes and the loop's
    // fallback tick still reads controls — degraded to slow, never deaf.
    let mut ctrl_rx: Option<tokio::sync::mpsc::Receiver<()>> = match std::env::var("DATABASE_URL") {
        Ok(url) => match records::listen_controls(&url, &bot_id).await {
            Ok(rx) => Some(rx),
            Err(e) => {
                eprintln!("control push unavailable ({e}) — falling back to polling");
                None
            }
        },
        Err(_) => None,
    };

    let feed_health = std::sync::Arc::new(std::sync::Mutex::new(live::FeedHealth {
        source: "starting".into(),
        ..Default::default()
    }));
    let (bar_tx, mut bar_rx) = tokio::sync::mpsc::channel::<RawBar>(64);
    match &trading {
        Trading::Ib(_) => {
            let mut feed_cfg = ib::IbConfig::from_env(live_account)?;
            feed_cfg.allow_orders = false; // the feed connection can never trade
            feed_cfg.client_id += 1;
            let feed = ib::IbVenue::connect(feed_cfg.clone())
                .await
                .map_err(|e| e.to_string())?;
            tokio::spawn(feed_task(
                feed,
                feed_cfg,
                bar_tx,
                feed_health.clone(),
                rec.clone(),
                bot_id.clone(),
            ));
        }
        Trading::Rithmic(v) => {
            // The history plant rides its own websocket inside the same
            // venue handle; polling it never contends with the order path.
            tokio::spawn(rithmic_feed_task(v.clone(), bar_tx, feed_health.clone()));
        }
    }
    eprintln!("run loop started ({mode}); ctrl-c to stop");

    // Never feed a bar the feature streams already consumed: warmup covers
    // the file, marks cover a restored session. Duplicates would corrupt
    // the incremental state (slots counted twice).
    let mut high_water: Option<chrono::DateTime<chrono::Utc>> = warmup.last().map(|b| b.ts_utc);
    for m in marks.values().flatten() {
        if Some(*m) > high_water {
            high_water = Some(*m);
        }
    }

    let mut last_bar_seen = chrono::Utc::now();
    // Flatten fires on the RISING EDGE of the operator asking for it, not on
    // the halt transition. Guarding it with "was not already halted" meant
    // Halt-then-Stop never flattened: the book was halted, so the Stop's
    // flatten was skipped and the position stayed open while the dashboard
    // said "the book closes at the bot's next cycle". Halt-then-Stop is the
    // 3am path the two buttons exist for — halt now, decide to be out a
    // minute later — so it is the one that must work.
    //
    // Starting false also means a process that comes up while stopped
    // flattens once, which is the correct reading of a stopped book.
    let mut flatten_asked = false;
    // Stop must not wait for the next evaluation. The two verbs differ in
    // how urgent they are, and the loop has to reflect that: Halt only
    // prevents the NEXT entry, so noticing it a cycle later costs nothing,
    // while Stop means "be flat", and every second between the press and
    // the order is market risk the operator thought they had cancelled.
    // The primary path is push (ctrl_rx, milliseconds); this tick is the
    // fallback for a dead listener, and the reason it exists at all.
    /// How long to wait before believing a broker/model disagreement. Orders
    /// placed on this bar are still propagating, and a degraded Gateway API
    /// answers "no positions" rather than erroring — so the first read can be
    /// wrong in exactly the direction that triggers a halt.
    const RECONCILE_SETTLE_S: u64 = 5;
    const CONTROL_TICK_S: u64 = 5;
    // Status publishing stays at a human cadence; only the control read
    // needs to be quick, and a status row every 5s is noise in the DB.
    const PUBLISH_EVERY_S: u64 = 60;
    let mut last_publish = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(PUBLISH_EVERY_S))
        .unwrap_or_else(std::time::Instant::now);
    loop {
        // Three wake sources, one loop body: a completed bar (evaluate), a
        // control notification (obey now), or the fallback tick (obey soon
        // even if the listener died). The last two take the same idle path.
        let next: Result<Option<RawBar>, ()> = tokio::select! {
            b = bar_rx.recv() => Ok(b),
            closed = async {
                match ctrl_rx.as_mut() {
                    Some(rx) => rx.recv().await.is_none(),
                    None => std::future::pending().await,
                }
            } => {
                if closed {
                    // Listener died: stop selecting on a closed channel
                    // (it would win every select) and live on the tick.
                    ctrl_rx = None;
                    eprintln!("control listener lost — polling controls instead");
                }
                Err(())
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(CONTROL_TICK_S)) => Err(()),
        };
        let done = match next {
            Err(_) => {
                // No completed bar lately. During the daily break, weekends
                // and holidays that is correct; while the market should be
                // printing, >70 minutes of silence is a stall: flatten
                // (live) and halt.
                let now = chrono::Utc::now();
                // Silence only counts while the market should be printing.
                // Without this the clock runs all weekend and the first
                // check after Sunday's open sees ~3700 quiet minutes and
                // kills the process before the first bar can possibly
                // arrive — the watchdog firing at exactly the moment the
                // bot is supposed to start working.
                if !live::market_should_be_open(now) {
                    last_bar_seen = now;
                }
                let silent = now.signed_duration_since(last_bar_seen).num_minutes();
                if live::market_should_be_open(now) && silent > 70 {
                    if live {
                        let _ = trading.flatten_all(&bot_id).await;
                    }
                    book.halted = Some("feed-stall".into());
                    live::publish_live(&rec, &book, &bot_id, mode, Some("feed stall — halted"))?;
                    return Err(format!(
                        "no completed bar for {silent}m in market hours — halted"
                    ));
                }
                // Controls are obeyed here as well, not only on a bar.
                // Reading them only when the feed ticks meant a Stop pressed
                // during the daily break, a weekend, or a quiet poll gap sat
                // unapplied — the book stayed open precisely when somebody
                // had just asked for it to be closed.
                let control = read_control(&rec, &bot_id)?;
                let halted_before = book.halted.clone();
                book.apply_control(&control);
                if control.flatten && !flatten_asked && live {
                    let _ = trading.flatten_all(&bot_id).await;
                }
                flatten_asked = control.flatten;
                // A control that changed the book is the one event worth
                // publishing the instant it happens. Reacting in 5s but
                // reporting on a 60s cadence looks identical, from the
                // dashboard, to nothing listening at all — the operator
                // presses Stop and watches "stopping" for a minute, which
                // is the limbo the short tick was supposed to remove.
                let changed = book.halted != halted_before;
                if changed || last_publish.elapsed().as_secs() >= PUBLISH_EVERY_S {
                    let fh = feed_health.lock().expect("feed health").clone();
                    live::publish_with_feed(&rec, &book, &bot_id, mode, None, Some(&fh))?;
                    last_publish = std::time::Instant::now();
                }
                // STOP ends the process, not just the trading. The book is
                // flat (the flatten above), the final state is published —
                // idling on from here would spend a Gateway connection and
                // a heartbeat on doing nothing. The api relaunches on
                // Start; a halt stays resident because winding an open
                // position down still requires evaluating bars.
                if control.flatten {
                    let _ = rec.log_line(
                        &bot_id,
                        "info",
                        "stopped by operator — process exiting; Start relaunches",
                    );
                    eprintln!("stopped by operator — process exiting");
                    return Ok(());
                }
                continue;
            }
            Ok(None) => {
                book.halted = Some("feed-closed".into());
                live::publish_live(&rec, &book, &bot_id, mode, Some("feed closed — halted"))?;
                return Err("the bar feed ended — halted".into());
            }
            Ok(Some(b)) => b,
        };
        last_bar_seen = chrono::Utc::now();
        if high_water.map(|h| done.ts_utc <= h).unwrap_or(false) {
            continue;
        }
        high_water = Some(done.ts_utc);

        // Controls, fresh every completed bar. Halt stops entries now and,
        // in live mode, flattens.
        let control = read_control(&rec, &bot_id)?;
        // Halt closes nothing; STOP closes. An earlier cut flattened on any
        // halt, which quietly made the two identical and made the
        // dashboard's promise ("halting does not close what is open") a lie.
        book.apply_control(&control);
        if control.flatten && !flatten_asked && live {
            let _ = trading.flatten_all(&bot_id).await;
        }
        flatten_asked = control.flatten;

        // Catch-up gate: the poll feed replays the last day's bars at
        // startup to rebuild today's session state. Those bars are history —
        // no ENTRY may fire on them (a stale decision filled now at market
        // is a different trade than the validated one). Exits on an open
        // position still run: shedding risk late beats holding it. Entries
        // unlock when bars are current.
        let stale = chrono::Utc::now()
            .signed_duration_since(done.ts_utc)
            .num_seconds()
            > 600;
        if stale {
            for st in &book.sleeves {
                book.disabled.insert(st.cfg.key.to_string());
            }
        }

        let outcome = live::drive_bar(&mut book, &mut globex, &mut rth, &frames, &done);

        if live {
            for (sleeve, direction, delta) in &outcome.transitions {
                if let Err(e) = live::mirror_transition(
                    trading.adapter(),
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
                    let _ = trading.flatten_all(&bot_id).await;
                    book.halted = Some(format!("order-refused: {e}"));
                    break;
                }
            }
            if outcome.session_rolled {
                // Broker-vs-model reconciliation at the boundary. Mismatch is
                // never auto-corrected: flatten and halt for a human.
                //
                // Read it TWICE. Orders placed moments earlier on this very bar
                // are still propagating — IB acknowledges the order before the
                // position query reflects it — and a Gateway whose API is
                // degraded answers "no positions" rather than failing. Either
                // way the first read can say zero while the account is not.
                // That cost a real halt: two entries fired on the roll bar, the
                // reconcile ran in the same iteration, and the book latched
                // `broker 0 vs model 2` against positions the broker was in the
                // middle of taking on.
                //
                // A genuine divergence persists; a race resolves. Confirming
                // costs a few seconds at a session boundary and nothing else.
                let model_net = live::model_net_contracts(&book);
                let mut venue_net = venue_net_contracts(&trading, &symbol).await?;
                if venue_net != model_net {
                    tokio::time::sleep(std::time::Duration::from_secs(RECONCILE_SETTLE_S)).await;
                    venue_net = venue_net_contracts(&trading, &symbol).await?;
                }
                if venue_net != model_net {
                    let _ = trading.flatten_all(&bot_id).await;
                    book.halted = Some(format!(
                        "reconcile-mismatch: broker {venue_net} vs model {model_net}"
                    ));
                }
            }
        }

        {
            let fh = feed_health.lock().expect("feed health").clone();
            live::publish_with_feed(&rec, &book, &bot_id, mode, None, Some(&fh))?;
            last_publish = std::time::Instant::now();
        }
        if book.halted.is_some() {
            eprintln!("halted: {:?} — loop stays up publishing state", book.halted);
        }
    }
}

async fn backfill(get: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    use ibapi::market_data::historical::BarTimestamp;
    use ibapi::prelude::{HistoricalBarSize, HistoricalWhatToShow};

    let path = get("--bars").ok_or("--bars is required")?;
    let days: i32 = get("--days").map(|d| d.parse().unwrap_or(5)).unwrap_or(5);

    // What "backfill" means depends on what is already there, and on a fresh
    // deployment the answer is "nothing": the bar cache is a cache, gitignored
    // on the laptop and absent on a new PVC, and the lab's Python exporter that
    // fills it does not exist in the image. So IB is also the SEED, not only
    // the tail — the same request either way.
    //
    // The stale case is the one that has to be got right rather than guessed:
    // appending to a cache whose last bar predates the window leaves a HOLE,
    // and a noise band computed across a hole is a number with no meaning. A
    // cache we cannot extend continuously is a cache we replace.
    let existing = if std::path::Path::new(&path).exists() {
        read_bars(&path).unwrap_or_default()
    } else {
        Vec::new()
    };
    let window_start = chrono::Utc::now() - chrono::Duration::days(days as i64);
    let last = match existing.last().map(|b| b.ts_utc) {
        None => {
            println!("no bar cache at {path} — seeding {days} days from IB");
            None
        }
        Some(last) if last < window_start => {
            println!(
                "bar cache ends at {last}, before the {days}-day window opens at {window_start} — \
                 re-seeding rather than leaving a gap"
            );
            None
        }
        Some(last) => Some(last),
    };

    let mut cfg = ib::IbConfig::from_env(false)?;
    // Not the trading loop's id. `futures-live.sh` runs this and then execs the
    // loop, and IB can hold a just-released client id reserved: sharing 9 would
    // make the bot's own warmup step the thing that kept it from starting.
    cfg.client_id = 7;
    let venue = ib::IbVenue::connect(cfg).await.map_err(|e| e.to_string())?;
    eprintln!("{}", venue.describe());
    let data = venue
        .client()
        .historical_data(venue.contract(), HistoricalBarSize::Min5)
        .what_to_show(HistoricalWhatToShow::Trades)
        .trading_hours(ibapi::prelude::TradingHours::Extended)
        .duration(ibapi::market_data::historical::Duration::days(days))
        .ending(time::OffsetDateTime::now_utc())
        .fetch()
        .await
        .map_err(|e| format!("historical data refused: {e}"))?;

    let mut fresh: Vec<RawBar> = Vec::new();
    for b in &data.bars {
        let BarTimestamp::DateTime(dt) = &b.date else {
            continue;
        };
        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(dt.unix_timestamp(), 0)
            .ok_or("bar timestamp out of range")?;
        if last.is_some_and(|l| ts <= l) {
            continue;
        }
        fresh.push(RawBar {
            ts_utc: ts,
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            volume: b.volume.max(0.0),
        });
    }
    fresh.sort_by_key(|b| b.ts_utc);
    if fresh.is_empty() {
        let Some(last) = last else {
            return Err(format!(
                "IB returned no 5-minute bars for the last {days} days, and there is no cache at \
                 {path} to fall back on — the warmup would be empty"
            ));
        };
        println!("nothing to append: file already ends at {last} and IB returned no newer bars");
        return Ok(());
    }
    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let seeding = last.is_none();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(!seeding)
        // Seeding REPLACES: the file either did not exist or held bars too old
        // to continue from, and both cases want exactly what IB just returned.
        .write(true)
        .truncate(seeding)
        .open(&path)
        .map_err(|e| format!("{path}: {e}"))?;
    for b in &fresh {
        let row = serde_json::json!({
            "ts_utc": b.ts_utc.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "open": b.open, "high": b.high, "low": b.low, "close": b.close,
            "volume": b.volume,
        });
        writeln!(f, "{row}").map_err(|e| e.to_string())?;
    }
    println!(
        "{} {} bars ({} -> {})",
        if seeding { "seeded" } else { "appended" },
        fresh.len(),
        fresh.first().expect("nonempty").ts_utc,
        fresh.last().expect("nonempty").ts_utc
    );
    Ok(())
}

/// The bar producer: realtime 5s bars when the subscription allows, else
/// poll IB historical 5-minute bars. Sends COMPLETED 5-minute buckets.
async fn feed_task(
    feed: ib::IbVenue,
    feed_cfg: ib::IbConfig,
    tx: tokio::sync::mpsc::Sender<RawBar>,
    health: std::sync::Arc<std::sync::Mutex<live::FeedHealth>>,
    rec: std::sync::Arc<records::blocking::Records>,
    bot_id: String,
) {
    /// How long a subscribed realtime stream may print nothing, during market
    /// hours, before we stop believing it. IB sends a 5-second bar every five
    /// seconds while the contract is trading, so two minutes is twenty-four
    /// missed bars — comfortably past a hiccup and far short of the 70-minute
    /// stall watchdog that would otherwise be the only thing to notice.
    const REALTIME_SILENCE_S: u64 = 120;
    /// How long the realtime path may run without producing a COMPLETED
    /// 5-minute bar. A stream that keeps printing is not the same as a feed
    /// that works: the bot ran ten minutes on a stream that delivered items
    /// the aggregator never turned into a single bucket, so the silence
    /// timeout above never fired and the loop had nothing to decide on. Two
    /// buckets' grace, then take the path that demonstrably works.
    const REALTIME_NO_BAR_S: u64 = 390;
    // Narrate state CHANGES only. A log that repeats itself every 20s is
    // one nobody reads, and it buries the line that mattered.
    let say = |level: &str, line: String| {
        eprintln!("feed: {line}");
        let _ = rec.log_line(&bot_id, level, &line);
    };
    // Attempt realtime first: subscribe and wait up to 45s for a first bar.
    match feed
        .client()
        .realtime_bars(feed.contract())
        .subscribe()
        .await
    {
        Ok(sub) => {
            let mut stream = sub.filter_data();
            match tokio::time::timeout(std::time::Duration::from_secs(45), stream.next()).await {
                Ok(Some(Ok(first))) => {
                    health.lock().expect("feed health").source = "realtime".into();
                    say("info", "realtime 5s bars streaming".into());
                    let mut agg = live::Aggregate5s::default();
                    let mut push = |b: &ibapi::market_data::realtime::Bar,
                                    agg: &mut live::Aggregate5s|
                     -> Option<RawBar> {
                        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(
                            b.date.unix_timestamp(),
                            0,
                        )?;
                        agg.on_bar(ts, b.open, b.high, b.low, b.close, b.volume)
                    };
                    if let Some(done) = push(&first, &mut agg) {
                        let _ = tx.send(done).await;
                    }
                    // A stream that ENDS is easy; a stream that stays open and
                    // stops printing is the one that cost 2h16m of blindness.
                    // `stream.next()` on a live-but-silent subscription parks
                    // forever, so the fallback below — which only ran when the
                    // stream ended — never fired, and the bot never returned to
                    // the polling path it had been using successfully an hour
                    // earlier. Silence is a failure mode, so it gets a clock.
                    // The stamp the aggregator buckets on, said once. A stream
                    // that prints for ten minutes without completing a single
                    // bucket can only be doing that because this value is not
                    // advancing, and it was not recoverable after the fact.
                    say(
                        "info",
                        format!("first realtime bar stamped {:?}", first.date),
                    );
                    let mut last_emit = std::time::Instant::now();
                    let why = loop {
                        let next = tokio::time::timeout(
                            std::time::Duration::from_secs(REALTIME_SILENCE_S),
                            stream.next(),
                        )
                        .await;
                        match next {
                            Ok(None) => break "realtime stream ended",
                            Ok(Some(item)) => {
                                let Ok(b) = item else { continue };
                                if let Some(done) = push(&b, &mut agg) {
                                    last_emit = std::time::Instant::now();
                                    if tx.send(done).await.is_err() {
                                        return;
                                    }
                                } else if last_emit.elapsed().as_secs() > REALTIME_NO_BAR_S
                                    && live::market_should_be_open(chrono::Utc::now())
                                {
                                    // Printing, but never completing a bar.
                                    break "realtime stream produced no completed bars";
                                }
                            }
                            // Out of hours a quiet stream is the market being
                            // shut, not the feed being broken. Only silence
                            // while the market should be printing is a fault.
                            Err(_) => {
                                if live::market_should_be_open(chrono::Utc::now()) {
                                    break "realtime stream went silent";
                                }
                            }
                        }
                    };
                    // Falling out of the task here would leave the process
                    // alive with no feed at all, so drop through to polling.
                    health.lock().expect("feed health").source = "poll".into();
                    say("warn", format!("{why} — falling back to polling"));
                }
                _ => {
                    health.lock().expect("feed health").source = "poll".into();
                    say(
                        "info",
                        "no realtime subscription — polling historical bars".into(),
                    );
                }
            }
        }
        Err(e) => {
            health.lock().expect("feed health").source = "poll".into();
            say(
                "warn",
                format!("realtime refused ({e}) — polling historical bars"),
            );
        }
    }

    // Poll mode: every 20s fetch the last day of 5-minute bars and emit the
    // buckets that completed since the last emission. A bucket is complete
    // once its 5 minutes have fully elapsed.
    use ibapi::market_data::historical::BarTimestamp;
    use ibapi::prelude::{HistoricalBarSize, HistoricalWhatToShow};
    let mut last_sent: Option<chrono::DateTime<chrono::Utc>> = None;
    // A timed-out fetch is not free. The one-shot historical API has no
    // cancel — dropping the future forgets the request locally while the
    // Gateway keeps it open forever — and IB stops answering entirely once
    // 50 historical requests are open, with no error to say so. So a run of
    // timeouts silently spends the budget and the feed never comes back.
    // Dropping the connection is what hands those slots back, so do it long
    // before the cap: three failures is a minute of silence, and reconnecting
    // costs one round trip.
    const RECONNECT_AFTER: u32 = 3;
    let mut feed = Some(feed);
    loop {
        // Don't ask a closed market for bars. IB answers a request over a
        // dead session by never answering at all, so polling through a
        // weekend is how the open-request budget got spent in the first
        // place: 50 unanswerable requests, then silence that outlives the
        // close. Idling here is also the honest reading — no bar is due.
        if !live::market_should_be_open(chrono::Utc::now()) {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            continue;
        }
        if feed.is_none() {
            match ib::IbVenue::connect(feed_cfg.clone()).await {
                Ok(v) => {
                    feed = Some(v);
                    say("info", "reconnected to the Gateway".into());
                }
                Err(e) => {
                    // Narrate the first refusal only; health carries the rest.
                    if health.lock().expect("feed health").failures == RECONNECT_AFTER {
                        say("error", format!("reconnect refused: {e}"));
                    }
                    health.lock().expect("feed health").failures += 1;
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                    continue;
                }
            }
        }
        let live_feed = feed.as_ref().expect("feed connected");
        // Bounded, like every other venue read. A Gateway that restarts
        // leaves a half-open socket, and an unbounded fetch parks on it
        // forever: the feed stops looping, stops reporting its own health,
        // and the process stays alive looking fine. A timeout turns that
        // silence into a failure the operator can see.
        let fetched = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            live_feed
                .client()
                .historical_data(live_feed.contract(), HistoricalBarSize::Min5)
                .what_to_show(HistoricalWhatToShow::Trades)
                .trading_hours(ibapi::prelude::TradingHours::Extended)
                .duration(ibapi::market_data::historical::Duration::days(1))
                .ending(time::OffsetDateTime::now_utc())
                .fetch(),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(ibapi::Error::Simple(
                "historical fetch timed out after 30s (Gateway restarting?)".into(),
            )),
        };
        match fetched {
            Ok(data) => {
                let recovered = {
                    let mut h = health.lock().expect("feed health");
                    let n = h.failures;
                    h.failures = 0;
                    h.last_error = None;
                    n
                };
                if recovered > 0 {
                    say(
                        "info",
                        format!("feed recovered after {recovered} failed poll(s)"),
                    );
                }
                let now = chrono::Utc::now();
                for b in &data.bars {
                    let BarTimestamp::DateTime(dt) = &b.date else {
                        continue;
                    };
                    let Some(ts) =
                        chrono::DateTime::<chrono::Utc>::from_timestamp(dt.unix_timestamp(), 0)
                    else {
                        continue;
                    };
                    let complete = ts + chrono::Duration::seconds(302) <= now;
                    let fresh = last_sent.map(|l| ts > l).unwrap_or(true);
                    if complete && fresh {
                        last_sent = Some(ts);
                        let bar = RawBar {
                            ts_utc: ts,
                            open: b.open,
                            high: b.high,
                            low: b.low,
                            close: b.close,
                            volume: b.volume.max(0.0),
                        };
                        health.lock().expect("feed health").last_bar_utc = Some(ts);
                        if tx.send(bar).await.is_err() {
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                let n = {
                    let mut h = health.lock().expect("feed health");
                    h.failures += 1;
                    h.last_error = Some(e.to_string());
                    h.failures
                };
                if n == 1 {
                    say("error", format!("historical poll failed: {e}"));
                }
                if n % RECONNECT_AFTER == 0 {
                    if n == RECONNECT_AFTER {
                        say(
                            "warn",
                            format!(
                                "{n} consecutive poll failures — dropping the Gateway \
                                 connection so IB releases the requests they left open"
                            ),
                        );
                    }
                    feed = None; // drop closes the socket; IB frees the slots
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    }
}

/// Read the control word. The canonical contract: no row, an unreadable
/// row, or an unknown state all mean HALTED — running is a deliberate act.
fn read_control(
    rec: &records::blocking::Records,
    bot_id: &str,
) -> Result<noise_book::runtime::Control, String> {
    let halted = || noise_book::runtime::Control {
        halt: true,
        ..Default::default()
    };
    Ok(rec
        .current_control(bot_id)
        .map_err(|e| e.to_string())?
        .map(|row| {
            serde_json::from_str(&row.payload)
                .map(|v: serde_json::Value| live::control_from_payload(&v))
                .unwrap_or_else(|_| halted())
        })
        .unwrap_or_else(halted))
}

/// The trading connection, venue-selected by the registry binding.
enum Trading {
    Ib(std::sync::Arc<ib::IbVenue>),
    Rithmic(std::sync::Arc<rithmic::RithmicVenue>),
}

impl Trading {
    fn describe(&self) -> String {
        match self {
            Trading::Ib(v) => v.describe(),
            Trading::Rithmic(v) => v.describe(),
        }
    }

    fn is_armed(&self) -> bool {
        match self {
            Trading::Ib(v) => v.is_armed(),
            Trading::Rithmic(v) => v.is_armed(),
        }
    }

    fn symbol_root(&self) -> String {
        std::env::var(match self {
            Trading::Ib(_) => "IB_SYMBOL",
            Trading::Rithmic(_) => "RITHMIC_SYMBOL",
        })
        .unwrap_or_else(|_| "MNQ".into())
    }

    fn adapter(&self) -> &dyn venue::VenueAdapter {
        match self {
            Trading::Ib(v) => v.as_ref(),
            Trading::Rithmic(v) => v.as_ref(),
        }
    }

    async fn flatten_all(&self, prefix: &str) -> Result<(), String> {
        match self {
            Trading::Ib(v) => v
                .flatten_all(prefix)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Trading::Rithmic(v) => v
                .flatten_all(prefix)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
        }
    }
}

/// Bar feed for the Rithmic venue: poll the history plant's 5-minute time
/// bars and emit newly completed buckets — the same bar-close discipline
/// as every other feed.
async fn rithmic_feed_task(
    venue: std::sync::Arc<rithmic::RithmicVenue>,
    tx: tokio::sync::mpsc::Sender<RawBar>,
    health: std::sync::Arc<std::sync::Mutex<live::FeedHealth>>,
) {
    health.lock().expect("feed health").source = "poll".into();
    let mut last_sent: Option<i64> = None;
    loop {
        let now = chrono::Utc::now().timestamp();
        let start = (now - 86_400) as i32;
        match venue.time_bars_5m(start, now as i32).await {
            Ok(bars) => {
                for (ts, o, h, l, c, v) in bars {
                    let complete = ts + 302 <= now;
                    let fresh = last_sent.map(|l| ts > l).unwrap_or(true);
                    if complete && fresh {
                        last_sent = Some(ts);
                        let Some(ts_utc) = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
                        else {
                            continue;
                        };
                        let bar = RawBar {
                            ts_utc,
                            open: o,
                            high: h,
                            low: l,
                            close: c,
                            volume: v.max(0.0),
                        };
                        if tx.send(bar).await.is_err() {
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                let mut h = health.lock().expect("feed health");
                h.failures += 1;
                h.last_error = Some(e.to_string());
                eprintln!("feed: rithmic history poll failed ({e}); retrying");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    }
}

async fn rithmic_check(get: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let live = get("--live").is_some() || std::env::args().any(|a| a == "--live");
    let cfg = rithmic::RithmicCfg::from_env(live)?;
    println!("connecting to Rithmic ({:?}) ...", cfg.env);
    let venue = rithmic::RithmicVenue::connect(cfg)
        .await
        .map_err(|e| e.to_string())?;
    println!("connected: {}", venue.describe());
    match venue::VenueAdapter::get_positions(&venue).await {
        Ok(p) if p.is_empty() => println!("positions: flat"),
        Ok(p) => {
            for pos in p {
                println!("position:  {} {} @ {}", pos.qty, pos.asset, pos.avg_price);
            }
        }
        Err(e) => println!("positions: UNAVAILABLE ({e})"),
    }
    let now = chrono::Utc::now().timestamp();
    match venue.time_bars_5m((now - 7_200) as i32, now as i32).await {
        Ok(bars) if bars.is_empty() => {
            println!("history:   reachable but no bars in the last 2h (market closed?)")
        }
        Ok(bars) => println!(
            "history:   {} bars in the last 2h (latest ts {})",
            bars.len(),
            bars.last().map(|b| b.0).unwrap_or(0)
        ),
        Err(e) => println!("history:   UNAVAILABLE ({e})"),
    }
    println!(
        "NOTE: order semantics (bracket with zero-tick children as plain entry) are \
         deliberately NOT probed here — verify with one manual 1-lot on the demo \
         account before arming a live loop."
    );
    Ok(())
}
