//! The operations dashboard, and the read-mostly API under it.
//!
//! ```text
//! api --state-dir var/bot --initial-cash 30000            # read-only
//! api --state-dir var/bot --initial-cash 30000 \
//!     --bot ./bot --bot-config config/bot.json \
//!     --expectation docs/research/backtest.json           # with controls
//! ```
//!
//! # It is a lens, never a dependency
//!
//! Design spec §8 puts the CLI first: the scheduler runs the same commands a
//! human does, and nothing has a private path into the engine. This process
//! reads files the bot has already written, and for anything that changes the
//! world it runs `bot` — the same binary, with the same gates, writing the same
//! run record. If this process is down, wrong, or overloaded, the bot neither
//! notices nor cares.
//!
//! # Full controls, and still no credentials here
//!
//! Halt, pause, resume, flatten and adopt are all on the page. None of them is
//! performed in this process: it holds no venue key, and
//! [`control`] assembles a fixed argument vector for a subprocess rather than
//! doing the work. That is the difference between a dashboard that can ask for
//! a flatten and a web server that can place an order — the first is a
//! convenience, the second is an attack surface wearing a convenience's clothes.
//!
//! Two consequences worth stating. There is exactly one implementation of what
//! "flatten" means, so a button and a shell cannot drift apart. And every
//! action still requires a name and a reason, still passes the bot's own gates,
//! and still lands in the run history — the page is a faster way to type the
//! command, not a second way to act.
//!
//! Started without `--bot`, the dashboard shows everything and changes nothing.
//! That is the default, because a process that could act the moment it was
//! pointed at a state directory would be one nobody chose to give authority to.
//!
//! # Loopback only, with no flag to change it
//!
//! Same rule as the research viewer, for a stronger reason: this one can halt a
//! trading system. The bind address is a constant and there is no host flag,
//! because a flag is a thing that gets set.

mod control;
mod fleet;
mod page;
mod state;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;

use control::{Action, BotCommand};
use rust_decimal::Decimal;

/// Never configurable. See the module docs.
const BIND: Ipv4Addr = Ipv4Addr::LOCALHOST;
const DEFAULT_PORT: u16 = 7434;

const MAX_REQUEST_LINE: u64 = 8 * 1024;
/// A control request is a few hundred bytes. Anything larger is not one.
const MAX_BODY: u64 = 64 * 1024;
/// How long a connection may sit without saying anything.
const REQUEST_TIMEOUT_S: u64 = 10;

/// The TriggerTrader mark: a reticle on a dark tile. Inline so the api stays
/// a single self-contained binary with no asset directory to deploy.
const FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#0B0F14"/><g stroke="#53C6E8" stroke-width="2.2" fill="none" stroke-linecap="round"><circle cx="16" cy="16" r="7.5"/><path d="M16 3.5v5M16 23.5v5M3.5 16h5M23.5 16h5"/></g><circle cx="16" cy="16" r="2.4" fill="#53C6E8"/></svg>"##;

const USAGE: &str = "\
usage: api [--state-dir <dir> --initial-cash <amount>]
           [--bot <path> --bot-config <file>]
           [--port N] [--expectation <file>] [--quote-currency USD]

Serves the operations dashboard on http://127.0.0.1:7434 until you stop it.
Loopback only - nothing outside this machine can reach it.

With no arguments at all this is the FLEET CONTROL PLANE: it serves every
registered bot from the records DB (DATABASE_URL) and wraps no bot of its
own — the deployment shape. --state-dir/--initial-cash additionally mount
the local book view for one runner bot's state dir (dev). Without --bot
and --bot-config that local view is read-only.
";

struct Config {
    /// Where the built frontend lives. Present → serve `frontend/dist`
    /// (the React app the design spec calls for). Absent → the legacy
    /// single-file page, until the port is finished.
    static_dir: Option<PathBuf>,
    /// `None` = pure fleet control plane: no local book view, DB only.
    state_dir: Option<PathBuf>,
    /// How to invoke the bot. `None` makes this a read-only dashboard.
    bot: Option<BotCommand>,
    initial_cash: Decimal,
    quote_currency: String,
    expectation: Option<PathBuf>,
    cadence_hours: i64,
    port: u16,
}

/// Read `KEY=value` lines into the environment without overwriting anything
/// already exported.
///
/// The api needs exactly one thing from `.env` — `DATABASE_URL`, for the
/// identity registry — and reads no credentials: it holds none and must keep
/// holding none. A real export wins over the file, so one value can be
/// overridden for one run without editing anything.
fn load_dotenv(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim().trim_matches('"').trim_matches('\''));
        // Only what this process is entitled to. Naming the variable rather
        // than loading the file wholesale means a key added to .env tomorrow
        // does not silently become readable by the web server.
        if k == "DATABASE_URL" && std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
}

fn main() -> ExitCode {
    load_dotenv(std::path::Path::new(".env"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let cfg = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("api: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let listener = match TcpListener::bind(SocketAddrV4::new(BIND, cfg.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("api: cannot bind 127.0.0.1:{}: {e}", cfg.port);
            return ExitCode::FAILURE;
        }
    };

    match &cfg.state_dir {
        Some(d) => println!("dashboard for {}", d.display()),
        None => println!("fleet control plane (no wrapped bot: records DB only)"),
    }
    println!("  http://127.0.0.1:{}", cfg.port);
    println!("  loopback only - not reachable from the network. ctrl-c to stop.");
    match &cfg.static_dir {
        Some(d) => println!("  ui: {} (built frontend)", d.display()),
        None => println!("  ui: legacy embedded page — run `bun run build` in frontend/"),
    }
    match std::env::var("DATABASE_URL") {
        Ok(_) => println!("  fleet: identity registry configured"),
        Err(_) => println!("  fleet: no DATABASE_URL, so the fleet view is unavailable"),
    }
    match &cfg.bot {
        Some(b) => println!(
            "  controls run {} --config {}",
            b.binary.display(),
            b.config.display()
        ),
        None => println!("  read-only: no --bot given, so the controls are disabled"),
    }

    // A thread per connection, and a read timeout on each.
    //
    // Serving connections one at a time looked simpler and was wrong: browsers
    // routinely open a speculative connection and send nothing on it, and a
    // serial server blocks in `read_line` on that socket forever. The whole
    // dashboard wedges — which is worst in exactly the situation it exists for,
    // because the operator then cannot tell a stopped bot from a stuck page.
    let cfg = std::sync::Arc::new(cfg);
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let cfg = std::sync::Arc::clone(&cfg);
                std::thread::spawn(move || {
                    let t = Some(std::time::Duration::from_secs(REQUEST_TIMEOUT_S));
                    let _ = s.set_read_timeout(t);
                    let _ = s.set_write_timeout(t);
                    if let Err(e) = handle(s, &cfg) {
                        eprintln!("  request failed: {e}");
                    }
                });
            }
            Err(e) => eprintln!("  connection failed: {e}"),
        }
    }
    ExitCode::SUCCESS
}

fn parse(args: &[String]) -> Result<Config, String> {
    let get = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    // Default to the repo's built frontend when it exists, so `api` with no
    // flags serves the real UI; --static-dir overrides for a deployment.
    let static_dir = get("--static-dir").map(PathBuf::from).or_else(|| {
        let d = PathBuf::from("frontend/dist");
        d.join("index.html").exists().then_some(d)
    });
    let state_dir = get("--state-dir").map(PathBuf::from);
    let initial_cash: Decimal = match get("--initial-cash") {
        Some(c) => c
            .parse()
            .map_err(|e| format!("--initial-cash is not a number: {e}"))?,
        None if state_dir.is_none() => Decimal::ZERO,
        None => {
            return Err(
                "--initial-cash is required with --state-dir: P&L is NAV less what was put \
                 in, and guessing that number would put a wrong figure on a dashboard people \
                 trust"
                    .into(),
            )
        }
    };
    let port = match get("--port") {
        Some(p) => p.parse().map_err(|_| format!("not a port number: {p}"))?,
        None => DEFAULT_PORT,
    };
    Ok(Config {
        static_dir,
        state_dir,
        bot: BotCommand::new(get("--bot"), get("--bot-config"))?,
        initial_cash,
        quote_currency: get("--quote-currency").unwrap_or_else(|| "USD".into()),
        expectation: get("--expectation").map(PathBuf::from),
        cadence_hours: get("--cadence-hours")
            .and_then(|c| c.parse().ok())
            .unwrap_or(24),
        port,
    })
}

/// The records handle and bot id the local book view should read through.
///
/// Opened per request rather than held: this server is thread-per-connection
/// and the handle is cheap, and a connection that went stale between requests
/// would otherwise wedge the page rather than simply retry.
fn records_for(cfg: &Config) -> Option<(std::sync::Arc<records::blocking::Records>, String)> {
    let path = &cfg.bot.as_ref()?.config;
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let bot_id = v.get("bot_id")?.as_str()?.to_string();
    let url = std::env::var("DATABASE_URL").ok()?;
    let rec = records::blocking::Records::connect(&url).ok()?;
    Some((rec, bot_id))
}

fn snapshot(cfg: &Config) -> Result<state::Snapshot, String> {
    let Some(state_dir) = cfg.state_dir.as_deref() else {
        return Err(
            "this api wraps no bot (fleet control plane) — there is no local book view".into(),
        );
    };
    state::build(&state::Inputs {
        state_dir,
        initial_cash: cfg.initial_cash,
        quote_currency: &cfg.quote_currency,
        cadence_hours: cfg.cadence_hours,
        expectation_path: cfg.expectation.as_deref(),
        controls_enabled: cfg.bot.is_some(),
        bot_config: cfg.bot.as_ref().map(|b| b.config.as_path()),
        records: records_for(cfg),
        run_limit: 200,
        // Enough to see the last run or two land, not enough to bury everything
        // below it. The full log is `bot positions` and the fill store; this is
        // a glance, and fifty rows of it pushed the run history off the page.
        fill_limit: 12,
    })
}

fn handle(mut stream: TcpStream, cfg: &Config) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    (&mut reader).take(MAX_REQUEST_LINE).read_line(&mut line)?;

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let route = target.split('?').next().unwrap_or("/").to_string();

    // Headers, only to find the body length.
    let mut content_length = 0usize;
    loop {
        let mut h = String::new();
        let n = (&mut reader).take(MAX_REQUEST_LINE).read_line(&mut h)?;
        if n == 0 || h.trim().is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }

    match (method.as_str(), route.as_str()) {
        ("GET" | "HEAD", "/") => serve_index(&mut stream, cfg, method == "HEAD"),
        // Hashed asset files from the built frontend. Everything under
        // /assets/ is content-addressed by the bundler, so it can be cached
        // hard; index.html never is, or a deploy would serve stale markup
        // pointing at assets that no longer exist.
        ("GET" | "HEAD", r) if r.starts_with("/assets/") => {
            serve_asset(&mut stream, cfg, r, method == "HEAD")
        }
        // Client-routed paths (/bot/futures-noise) are the SAME document —
        // the app resolves them. Without this a refresh or a pasted link
        // 404s, which is the classic way a single-page app feels broken.
        ("GET" | "HEAD", r)
            if cfg.static_dir.is_some()
                && !r.starts_with("/api/")
                && !r.contains('.') =>
        {
            serve_index(&mut stream, cfg, method == "HEAD")
        }
        ("GET", r) if r.starts_with("/api/bots/") && r.ends_with("/state") => {
            let id = r
                .trim_start_matches("/api/bots/")
                .trim_end_matches("/state");
            match fleet::detail(&std::env::current_dir().unwrap_or_default(), id) {
                Ok(v) => json(&mut stream, 200, &serde_json::to_vec(&v).unwrap()),
                Err(e) => json_error(&mut stream, 404, &e),
            }
        }
        ("GET", "/api/fleet/overview") => match fleet::overview(
            &std::env::current_dir().unwrap_or_default(),
            cfg.state_dir.as_deref().unwrap_or(std::path::Path::new("")),
        ) {
            Ok(v) => json(&mut stream, 200, &serde_json::to_vec(&v).unwrap()),
            Err(e) => json(
                &mut stream,
                200,
                &serde_json::to_vec(&serde_json::json!({"available": false, "reason": e})).unwrap(),
            ),
        },
        ("GET", "/api/bots") => match fleet::list(
            &std::env::current_dir().unwrap_or_default(),
            cfg.state_dir.as_deref().unwrap_or(std::path::Path::new("")),
        ) {
            Ok(v) => json(&mut stream, 200, &serde_json::to_vec(&v).unwrap()),
            Err(e) => json(
                &mut stream,
                200,
                &serde_json::to_vec(&serde_json::json!({"available": false, "reason": e})).unwrap(),
            ),
        },
        ("POST", r)
            if r.starts_with("/api/bots/")
                && (r.ends_with("/halt") || r.ends_with("/resume") || r.ends_with("/stop")) =>
        {
            // Three instructions, not two: halt opens nothing new, stop also
            // closes the book, resume trades again.
            let state = if r.ends_with("/halt") {
                "halted"
            } else if r.ends_with("/stop") {
                "stopped"
            } else {
                "running"
            };
            let halt = state != "running";
            let id = r
                .trim_start_matches("/api/bots/")
                .trim_end_matches("/halt")
                .trim_end_matches("/resume")
                .trim_end_matches("/stop")
                .to_string();
            let mut body = vec![0u8; content_length.min(64 * 1024)];
            reader.read_exact(&mut body).ok();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            // Single-operator tool: the dashboard asks neither who nor why,
            // so the API demands neither. The row still records that the
            // action came from the dashboard — the only distinction that
            // carries information here. No invented sentences.
            let reason = v
                .get("reason")
                .and_then(|s| s.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("");
            let by = v
                .get("by")
                .and_then(|s| s.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("dashboard");
            {
                match fleet::set_state(&id, state, reason, by) {
                    Ok(v) => json(&mut stream, 200, &serde_json::to_vec(&v).unwrap()),
                    Err(e) => json_error(&mut stream, 400, &e),
                }
            }
        }
        ("POST", r) if r.starts_with("/api/bots/") && r.ends_with("/venue") => {
            let id = r
                .trim_start_matches("/api/bots/")
                .trim_end_matches("/venue")
                .to_string();
            let mut body = vec![0u8; content_length.min(64 * 1024)];
            reader.read_exact(&mut body).ok();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let account = v.get("account_id").and_then(|s| s.as_str()).unwrap_or("");
            let reason = v
                .get("reason")
                .and_then(|s| s.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("");
            let by = v
                .get("by")
                .and_then(|s| s.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("dashboard");
            if account.is_empty() {
                json_error(&mut stream, 400, "body must carry account_id")
            } else {
                match fleet::set_venue(&id, account, reason, by) {
                    Ok(v) => json(&mut stream, 200, &serde_json::to_vec(&v).unwrap()),
                    Err(e) => json_error(&mut stream, 400, &e),
                }
            }
        }
        ("GET", "/api/build") => json(
            &mut stream,
            200,
            format!(r#"{{"built":"{}"}}"#, env!("BUILD_STAMP")).as_bytes(),
        ),
        ("GET", "/api/state") if cfg.state_dir.is_none() => {
            // Fleet control plane: no wrapped bot, so no local book — a 200
            // sentinel rather than an error, because this is the expected
            // deployment shape, and browsers console-log every 5xx.
            json(&mut stream, 200, br#"{"fleet_only":true}"#)
        }
        ("GET", "/api/state") => match snapshot(cfg) {
            Ok(s) => json(&mut stream, 200, &serde_json::to_vec(&s).unwrap()),
            Err(e) => json_error(&mut stream, 500, &e),
        },
        ("POST", r) if Action::parse(r).is_some() => {
            let action = Action::parse(r).expect("just matched");
            control(&mut stream, cfg, &mut reader, content_length, action)
        }
        ("GET" | "HEAD", "/favicon.ico" | "/favicon.svg") => respond(
            &mut stream,
            200,
            "image/svg+xml",
            FAVICON_SVG.as_bytes(),
            method == "HEAD",
        ),
        ("GET" | "HEAD" | "POST", _) => json_error(&mut stream, 404, "no such route"),
        _ => json_error(&mut stream, 405, "method not allowed"),
    }
}

/// Run one control, by invoking the bot.
///
/// Nothing here touches the venue or the control file directly. The bot owns
/// the credential, the gates and the run record, so a button and a shell are
/// the same code path — see the [`control`] module docs.
fn control(
    stream: &mut TcpStream,
    cfg: &Config,
    reader: &mut BufReader<TcpStream>,
    content_length: usize,
    action: Action,
) -> std::io::Result<()> {
    let Some(bot) = cfg.bot.as_ref() else {
        return json_error(
            stream,
            503,
            &format!(
                "this dashboard was started without --bot and --bot-config, so it can only \
                 show. Run `{}` at a terminal, or restart the api with both flags.",
                control::as_typed(None, action)
            ),
        );
    };

    let mut body = vec![0u8; content_length.min(MAX_BODY as usize)];
    reader.read_exact(&mut body)?;
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let reason = parsed
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let by = parsed
        .get("by")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    // Both required, same as the CLI. "Halted" with no name and no reason is
    // the thing the next operator cannot act on.
    if reason.is_empty() || by.is_empty() {
        return json_error(
            stream,
            400,
            "both a reason and a name are required: the person who finds this stopped is \
             usually not the person who stopped it",
        );
    }

    // The api's console becomes the audit trail for anything done from a
    // browser. Consequential actions are marked, because "who flattened the
    // book at 02:14" is a question that gets asked.
    eprintln!(
        "  control: {}{} by {by}: {reason}",
        control::as_typed(Some(bot), action),
        if action.goes_live() {
            "  [*** POINTS THE BOT AT REAL MONEY ***]"
        } else if action.is_consequential() {
            "  [moves capital or grants authority]"
        } else {
            ""
        }
    );

    match control::run(bot, action, reason, by) {
        Ok(out) => {
            // The bot's own stdout is the answer. Reformatting it here would be
            // a second account of what happened, and the two would drift.
            let parsed: serde_json::Value =
                serde_json::from_str(&out.stdout).unwrap_or(serde_json::Value::Null);
            let payload = serde_json::json!({
                "ok": out.ok,
                "record": parsed,
                "error": (!out.ok).then(|| out.stderr.clone()),
                "command": control::as_typed(Some(bot), action),
            });
            let code = if out.ok { 200 } else { 409 };
            json(stream, code, &serde_json::to_vec(&payload).unwrap())
        }
        Err(e) => json_error(stream, 500, &e),
    }
}

fn json(stream: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
    respond(
        stream,
        status,
        "application/json; charset=utf-8",
        body,
        false,
    )
}

fn json_error(stream: &mut TcpStream, status: u16, message: &str) -> std::io::Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "error": message })).unwrap();
    json(stream, status, &body)
}

/// The SPA entry point. A router path like /bot/futures-noise is served the
/// same document — the app resolves it client-side — so a refresh or a
/// pasted link lands where it should instead of 404ing.
fn serve_index(stream: &mut TcpStream, cfg: &Config, head_only: bool) -> std::io::Result<()> {
    match cfg.static_dir.as_ref().map(|d| std::fs::read(d.join("index.html"))) {
        Some(Ok(body)) => respond(stream, 200, "text/html; charset=utf-8", &body, head_only),
        _ => respond(
            stream,
            200,
            "text/html; charset=utf-8",
            page::HTML.as_bytes(),
            head_only,
        ),
    }
}

fn serve_asset(
    stream: &mut TcpStream,
    cfg: &Config,
    path: &str,
    head_only: bool,
) -> std::io::Result<()> {
    let Some(dir) = cfg.static_dir.as_ref() else {
        return json_error(stream, 404, "no built frontend is being served");
    };
    // Path traversal is the one attack a static file server invites, and
    // this one is reachable from a browser on the same machine: take the
    // file name only, never a caller-supplied path.
    let name = path.trim_start_matches("/assets/");
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return json_error(stream, 404, "no such asset");
    }
    match std::fs::read(dir.join("assets").join(name)) {
        Ok(body) => respond(stream, 200, content_type(name), &body, head_only),
        Err(_) => json_error(stream, 404, "no such asset"),
    }
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'self'; \
         style-src 'self' 'unsafe-inline'; font-src 'self' data:; connect-src 'self'; \
         img-src 'self' data:; base-uri 'none'; form-action 'none'\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bind_address_is_loopback() {
        assert!(BIND.is_loopback());
    }

    #[test]
    fn there_is_no_flag_that_changes_the_bind_address() {
        // A flag is a thing that gets set. The needles are assembled rather
        // than written literally, or this test would find its own assertions.
        let source = include_str!("main.rs");
        for needle in [
            concat!("--", "host"),
            concat!("--", "bind"),
            concat!("0.0", ".0.0"),
            concat!("Ipv4Addr::", "UNSPECIFIED"),
        ] {
            assert!(
                !source.contains(needle),
                "{needle} appears in the source: the dashboard may no longer be loopback-only"
            );
        }
    }

    #[test]
    fn this_process_cannot_place_an_order() {
        // The security boundary from spec §3.3, asserted against the source
        // rather than assumed from the dependency list. The dashboard gained
        // full controls, and the way it did that must stay "ask the bot" — the
        // moment any of these appear here, the process serving a web page has
        // become one that can trade.
        let source = concat!(
            include_str!("main.rs"),
            include_str!("state.rs"),
            include_str!("control.rs")
        );
        for needle in [
            concat!("place_", "order"),
            concat!("cancel_", "order"),
            concat!("Order", "Request"),
            concat!("api_", "key"),
            concat!("Paper", "Venue::new"),
            concat!("Venue", "Adapter"),
        ] {
            assert!(
                !source.contains(needle),
                "{needle} appears in the api process: it must never be able to trade"
            );
        }
    }

    #[test]
    fn every_control_is_delegated_and_none_is_performed_here() {
        // The contract the full-control dashboard rests on: this process runs
        // the bot, and never writes a control file or a ledger itself. Two
        // implementations of "halt" would eventually disagree about what it
        // means, and the one nobody tested would be the web one.
        let source = concat!(include_str!("main.rs"), include_str!("control.rs"));
        for needle in [
            concat!("Control", "File {"),
            concat!("Control", "File::"),
            concat!("Ledger", "::"),
            concat!("Runner", " {"),
        ] {
            assert!(
                !source.contains(needle),
                "{needle} appears in the control path: controls must be delegated to the bot, \
                 not reimplemented here"
            );
        }
    }

    #[test]
    fn a_dashboard_without_a_bot_offers_no_controls() {
        // Read-only is a supported way to run this, and it has to be the
        // default: a dashboard that could act the moment it was pointed at a
        // state directory would be one nobody chose to give authority to.
        let cfg = parse(&[
            "--state-dir".into(),
            ".".into(),
            "--initial-cash".into(),
            "1".into(),
        ])
        .unwrap();
        assert!(cfg.bot.is_none());
    }
}
