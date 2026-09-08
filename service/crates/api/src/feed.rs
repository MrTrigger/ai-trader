//! Current feed health comes from the poller, independently of daily runs.
use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;

#[derive(Deserialize)]
struct Poll {
    source: String,
    updated_at: Option<String>,
    failures: u32,
    last_error: Option<String>,
}

fn status(poll: Poll, expected_source: Option<&str>, now: OffsetDateTime) -> Value {
    let elapsed = poll
        .updated_at
        .as_deref()
        .and_then(|s| OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok())
        .map(|t| (now - t).whole_seconds());
    let mismatched = expected_source.is_some_and(|source| source != poll.source);
    let healthy =
        !mismatched && poll.failures == 0 && elapsed.is_some_and(|age| (0..=180).contains(&age));
    let error = if mismatched {
        Some("feed source differs from the bound venue".to_string())
    } else if elapsed.is_some_and(|age| age < 0) {
        Some("feed success timestamp is in the future".to_string())
    } else {
        poll.last_error
    };
    json!({
        "source": poll.source,
        "last_bar_age_seconds": elapsed.map(|age| age.max(0)),
        "failures": poll.failures,
        "last_error": error,
        "market_open": true,
        "healthy": healthy,
    })
}

pub fn read(root: &Path, state_dir: Option<&str>, expected_source: Option<&str>) -> Option<Value> {
    let path = root.join(state_dir?).join("feed.json");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(unreadable()),
    };
    Some(match serde_json::from_slice(&bytes) {
        Ok(poll) => status(poll, expected_source, OffsetDateTime::now_utc()),
        Err(_) => unreadable(),
    })
}

fn unreadable() -> Value {
    json!({"healthy": false, "market_open": true, "last_bar_age_seconds": null,
           "failures": 1, "last_error": "feed metadata is unreadable"})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn poll(age: i64, failures: u32) -> Poll {
        Poll {
            source: "lighter".into(),
            updated_at: Some(
                (OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1000 - age))
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap(),
            ),
            failures,
            last_error: None,
        }
    }
    #[test]
    fn freshness_comes_from_last_success_and_bound_venue() {
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1000);
        assert_eq!(status(poll(180, 0), Some("lighter"), now)["healthy"], true);
        for (age, failures) in [(181, 0), (20, 1), (-1, 0)] {
            assert_eq!(
                status(poll(age, failures), Some("lighter"), now)["healthy"],
                false
            );
        }
        assert_eq!(
            status(poll(0, 0), Some("hyperliquid"), now)["healthy"],
            false
        );
        let mut missing = poll(0, 0);
        missing.updated_at = None;
        assert_eq!(status(missing, Some("lighter"), now)["healthy"], false);
    }
    #[test]
    fn each_bot_reads_its_own_poller_and_missing_stays_unknown() {
        let root = std::env::temp_dir().join(format!("api-feed-{}", std::process::id()));
        std::fs::create_dir_all(root.join("lighter")).unwrap();
        std::fs::write(
            root.join("lighter/feed.json"),
            r#"{"source":"lighter","updated_at":null,"failures":3,"last_error":"timeout"}"#,
        )
        .unwrap();
        assert_eq!(
            read(&root, Some("lighter"), Some("lighter")).unwrap()["failures"],
            3
        );
        assert!(read(&root, Some("hyperliquid"), Some("hyperliquid")).is_none());
        assert!(read(&root, None, Some("lighter")).is_none());
        std::fs::write(root.join("lighter/feed.json"), "{}").unwrap();
        assert_eq!(
            read(&root, Some("lighter"), Some("lighter")).unwrap()["healthy"],
            false
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
