//! Serve the research view on this machine, and only on this machine.
//!
//! ```text
//! cargo run -p viewer -- docs/research/research.html
//! cargo run -p viewer -- docs/research/research.html --port 8080
//! ```
//!
//! **Loopback only, and not as a default that can be overridden.** The bind
//! address is hardcoded to `127.0.0.1`; there is no flag to change it, because
//! a flag is a thing that gets set. Design spec §1 puts this project on a
//! different security footing from the journal — the journal is LAN-gated
//! behind `envoy-internal`, this one will hold venue credentials and "must
//! never share that boundary". A research viewer is not that process, but it
//! lives in the same repo and the habit should not be "bind wide, restrict
//! later".
//!
//! **No dependencies.** One local file to one local browser does not need a web
//! framework, and the smallest thing that works is the thing least likely to
//! grow into an accidental public surface. This is *not* the `api` process from
//! §3.3 — that one is axum, holds no credentials, and serves stored state. This
//! serves a file.
//!
//! The file is re-read on every request, so regenerating the HTML and hitting
//! refresh shows the new one. That is the whole development loop.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Never configurable. See the module docs.
const BIND: Ipv4Addr = Ipv4Addr::LOCALHOST;
const DEFAULT_PORT: u16 = 7433;

/// A request line longer than this is not a browser asking for a file.
const MAX_REQUEST_LINE: u64 = 8 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!(
            "usage: viewer <file.html> [--port N]\n\n\
             Serves one file on http://127.0.0.1:{DEFAULT_PORT} until you stop it.\n\
             Loopback only - nothing outside this machine can reach it."
        );
        return ExitCode::from(if args.is_empty() { 2 } else { 0 });
    }

    let path = PathBuf::from(&args[0]);
    let port = match parse_port(&args) {
        Ok(p) => p,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    if !path.is_file() {
        eprintln!("no such file: {}", path.display());
        eprintln!(
            "provide a generated self-contained research HTML file at {}",
            path.display()
        );
        return ExitCode::from(2);
    }

    let listener = match TcpListener::bind(SocketAddrV4::new(BIND, port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot bind 127.0.0.1:{port}: {e}");
            if port == DEFAULT_PORT {
                eprintln!("something else is using it; pass --port N for another");
            }
            return ExitCode::from(1);
        }
    };

    println!("serving {}", path.display());
    println!("  http://127.0.0.1:{port}");
    println!("  loopback only - not reachable from the network. ctrl-c to stop.");

    for stream in listener.incoming() {
        match stream {
            // One connection at a time is right here: a single browser reading
            // a single page. Concurrency would be complexity with no user.
            Ok(s) => {
                if let Err(e) = handle(s, &path) {
                    eprintln!("  request failed: {e}");
                }
            }
            Err(e) => eprintln!("  connection failed: {e}"),
        }
    }
    ExitCode::SUCCESS
}

fn parse_port(args: &[String]) -> Result<u16, String> {
    let Some(i) = args.iter().position(|a| a == "--port") else {
        return Ok(DEFAULT_PORT);
    };
    let value = args.get(i + 1).ok_or("--port needs a number")?;
    value
        .parse::<u16>()
        .map_err(|_| format!("not a port number: {value}"))
        .and_then(|p| {
            if p == 0 {
                Err("port 0 is not useful here".into())
            } else {
                Ok(p)
            }
        })
}

fn handle(mut stream: TcpStream, path: &Path) -> std::io::Result<()> {
    let mut line = String::new();
    BufReader::new(&stream)
        .take(MAX_REQUEST_LINE)
        .read_line(&mut line)?;

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");

    // GET and HEAD only. Nothing here has state to change, so anything else is
    // either a mistake or a probe, and both deserve the same flat refusal.
    if method != "GET" && method != "HEAD" {
        return respond(
            &mut stream,
            405,
            "text/plain; charset=utf-8",
            b"method not allowed",
            false,
        );
    }

    // One file, served at any path. There is no directory to traverse, which is
    // the simplest possible answer to path traversal: the request path is never
    // used to resolve anything.
    let route = target.split('?').next().unwrap_or("/");
    if route == "/favicon.ico" {
        return respond(&mut stream, 404, "text/plain; charset=utf-8", b"", false);
    }

    let body = fs::read(path)?;
    respond(
        &mut stream,
        200,
        "text/html; charset=utf-8",
        &body,
        method == "HEAD",
    )
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
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
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
        // The property this crate exists to hold. If this ever changes, the
        // research view became reachable from the network.
        assert_eq!(BIND, Ipv4Addr::new(127, 0, 0, 1));
        assert!(BIND.is_loopback());
    }

    #[test]
    fn there_is_no_flag_that_changes_the_bind_address() {
        // A flag is a thing that gets set. Grep the source rather than trusting
        // the constant: this catches a future host flag being added.
        //
        // The needles are assembled rather than written literally, or this test
        // would find its own assertions and fail on the day it was written.
        let source = include_str!("main.rs");
        for needle in [
            concat!("--", "host"),
            concat!("--", "bind"),
            concat!("0.0", ".0.0"),
            concat!("Ipv4Addr::", "UNSPECIFIED"),
        ] {
            assert!(
                !source.contains(needle),
                "{needle} appears in the source: the viewer may no longer be loopback-only"
            );
        }
    }

    #[test]
    fn the_port_defaults_and_parses() {
        assert_eq!(parse_port(&["x.html".into()]).unwrap(), DEFAULT_PORT);
        assert_eq!(
            parse_port(&["x.html".into(), "--port".into(), "9000".into()]).unwrap(),
            9000
        );
    }

    #[test]
    fn a_bad_port_is_refused_rather_than_defaulted() {
        // Silently falling back would serve on a port the caller did not ask
        // for and did not expect to be listening.
        assert!(parse_port(&["x".into(), "--port".into(), "nope".into()]).is_err());
        assert!(parse_port(&["x".into(), "--port".into(), "0".into()]).is_err());
        assert!(parse_port(&["x".into(), "--port".into()]).is_err());
    }
}
