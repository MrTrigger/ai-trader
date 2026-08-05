//! The operations dashboard, and the read-mostly API under it.
//!
//! ```text
//! api --state-dir var/bot --initial-cash 30000
//! api --state-dir var/bot --initial-cash 30000 --port 7434 \
//!     --expectation docs/research/backtest.json
//! ```
//!
//! # It is a lens, never a dependency
//!
//! Design spec §8 puts the CLI first: the scheduler runs the same commands a
//! human does, and nothing has a private path into the engine. This process
//! reads files the bot has already written and holds no venue credentials, so
//! it cannot place an order however it is asked. If it is down, wrong, or
//! overloaded, the bot neither notices nor cares.
//!
//! # The dashboard can stop trading; it cannot start it
//!
//! Halt and pause reduce what the system is permitted to do, and both are
//! reachable from the page. Resume and flatten are not: resume grants authority
//! and flatten moves capital, and neither should be one mis-click away in a
//! browser tab that has been open since Tuesday. The page shows the exact CLI
//! command instead.
//!
//! That asymmetry is the point — the emergency control is always available, and
//! the recovery control requires a human at a terminal who has looked at why it
//! stopped.
//!
//! # Loopback only, with no flag to change it
//!
//! Same rule as the research viewer, for a stronger reason: this one can halt a
//! trading system. The bind address is a constant and there is no host flag,
//! because a flag is a thing that gets set.

mod page;
mod state;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;

use runner::ControlFile;
use rust_decimal::Decimal;

/// Never configurable. See the module docs.
const BIND: Ipv4Addr = Ipv4Addr::LOCALHOST;
const DEFAULT_PORT: u16 = 7434;

const MAX_REQUEST_LINE: u64 = 8 * 1024;
/// A control request is a few hundred bytes. Anything larger is not one.
const MAX_BODY: u64 = 64 * 1024;
/// How long a connection may sit without saying anything.
const REQUEST_TIMEOUT_S: u64 = 10;

const USAGE: &str = "\
usage: api --state-dir <dir> --initial-cash <amount> [--port N] [--expectation <file>]

Serves the operations dashboard on http://127.0.0.1:7434 until you stop it.
Loopback only - nothing outside this machine can reach it.
";

struct Config {
    state_dir: PathBuf,
    initial_cash: Decimal,
    expectation: Option<PathBuf>,
    cadence_hours: i64,
    port: u16,
}

fn main() -> ExitCode {
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

    println!("dashboard for {}", cfg.state_dir.display());
    println!("  http://127.0.0.1:{}", cfg.port);
    println!("  loopback only - not reachable from the network. ctrl-c to stop.");

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
    let state_dir = PathBuf::from(get("--state-dir").ok_or("--state-dir is required")?);
    let initial_cash: Decimal = get("--initial-cash")
        .ok_or(
            "--initial-cash is required: P&L is NAV less what was put in, and guessing that \
             number would put a wrong figure on a dashboard people trust",
        )?
        .parse()
        .map_err(|e| format!("--initial-cash is not a number: {e}"))?;
    let port = match get("--port") {
        Some(p) => p.parse().map_err(|_| format!("not a port number: {p}"))?,
        None => DEFAULT_PORT,
    };
    Ok(Config {
        state_dir,
        initial_cash,
        expectation: get("--expectation").map(PathBuf::from),
        cadence_hours: get("--cadence-hours")
            .and_then(|c| c.parse().ok())
            .unwrap_or(24),
        port,
    })
}

fn snapshot(cfg: &Config) -> Result<state::Snapshot, String> {
    state::build(&state::Inputs {
        state_dir: &cfg.state_dir,
        initial_cash: cfg.initial_cash,
        cadence_hours: cfg.cadence_hours,
        expectation_path: cfg.expectation.as_deref(),
        run_limit: 200,
        fill_limit: 50,
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
        ("GET" | "HEAD", "/") => respond(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            page::HTML.as_bytes(),
            method == "HEAD",
        ),
        ("GET", "/api/state") => match snapshot(cfg) {
            Ok(s) => json(&mut stream, 200, &serde_json::to_vec(&s).unwrap()),
            Err(e) => json_error(&mut stream, 500, &e),
        },
        ("POST", "/api/halt") => {
            control(&mut stream, cfg, &mut reader, content_length, true, false)
        }
        ("POST", "/api/pause") => {
            control(&mut stream, cfg, &mut reader, content_length, false, true)
        }
        // Deliberately absent: resume and flatten. See the module docs.
        ("POST", "/api/resume" | "/api/flatten") => json_error(
            &mut stream,
            403,
            "resume and flatten are CLI-only. This process holds no venue credentials and will \
             not grant trading authority from a browser tab; run `bot --config <file> resume` or \
             `bot --config <file> flatten --confirm` at a terminal.",
        ),
        ("GET" | "HEAD", "/favicon.ico") => {
            respond(&mut stream, 404, "text/plain; charset=utf-8", b"", true)
        }
        ("GET" | "HEAD" | "POST", _) => json_error(&mut stream, 404, "no such route"),
        _ => json_error(&mut stream, 405, "method not allowed"),
    }
}

/// Write a control file. The only mutation this process performs, and both
/// callers reduce what the system may do.
fn control(
    stream: &mut TcpStream,
    cfg: &Config,
    reader: &mut BufReader<TcpStream>,
    content_length: usize,
    kill_switch: bool,
    paused: bool,
) -> std::io::Result<()> {
    let mut body = vec![0u8; content_length.min(MAX_BODY as usize)];
    reader.read_exact(&mut body)?;
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let reason = parsed.get("reason").and_then(|v| v.as_str()).unwrap_or("");
    let by = parsed.get("by").and_then(|v| v.as_str()).unwrap_or("");

    // Both required, same as the CLI. "Halted" with no name and no reason is
    // the thing the next operator cannot act on.
    if reason.trim().is_empty() || by.trim().is_empty() {
        return json_error(
            stream,
            400,
            "both a reason and a name are required: the person who finds this stopped is \
             usually not the person who stopped it",
        );
    }

    let control = ControlFile {
        kill_switch,
        paused,
        reason: Some(reason.trim().to_string()),
        set_by: Some(by.trim().to_string()),
        set_at: Some(
            time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        ),
    };
    match control.write(&cfg.state_dir.join("controls.json")) {
        Ok(()) => json(stream, 200, &serde_json::to_vec(&control).unwrap()),
        Err(e) => json_error(stream, 500, &e.to_string()),
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
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; \
         script-src 'unsafe-inline'; connect-src 'self'\r\n\
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
        // rather than assumed from the dependency list: `api` holds no venue
        // credentials, so nothing here may reach an order-placing call.
        let source = concat!(include_str!("main.rs"), include_str!("state.rs"));
        for needle in [
            concat!("place_", "order"),
            concat!("cancel_", "order"),
            concat!("Order", "Request"),
            concat!("api_", "key"),
            concat!("Paper", "Venue::new"),
        ] {
            assert!(
                !source.contains(needle),
                "{needle} appears in the api process: it must never be able to trade"
            );
        }
    }

    #[test]
    fn resume_and_flatten_are_not_reachable_over_http() {
        // The asymmetry the dashboard is built on: it can always stop trading,
        // and can never start it.
        let source = include_str!("main.rs");
        assert!(source.contains("(\"POST\", \"/api/resume\" | \"/api/flatten\")"));
        assert!(
            !source.contains("kill_switch: false,\n        paused: false"),
            "nothing in this process may write a control file that permits trading"
        );
    }
}
