//! Market calendars (spec section 4.3, built in identity step 3).
//!
//! "Crypto never closes" was baked into the planner as an assumption; a
//! futures bot cannot share that assumption, so the calendar becomes an
//! interface. Implementations are DATA about a market's clock — nothing here
//! decides anything.

use time::{Duration, OffsetDateTime, Weekday};

/// A market's trading clock.
pub trait Calendar: Send + Sync {
    /// Whether the market trades at this instant.
    fn is_open(&self, ts: OffsetDateTime) -> bool;
    /// A human name for logs and dashboards.
    fn name(&self) -> &'static str;
}

/// Crypto: always open. The planner's historical assumption, made explicit.
pub struct AlwaysOpen;

impl Calendar for AlwaysOpen {
    fn is_open(&self, _ts: OffsetDateTime) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "always-open"
    }
}

/// CME Globex equity-index schedule, expressed in UTC offsets from the
/// exchange's own clock (America/Chicago). Approximations, stated:
///
/// * Sunday 17:00 CT open -> Friday 16:00 CT close.
/// * Daily maintenance halt 16:00-17:00 CT.
/// * Exchange holidays are NOT modelled here yet — holiday data is a data
///   feed, not a formula, and arrives with the futures bot's data source.
///   Until then a holiday reads as "open", which fails SAFE for a feed
///   (no bars arrive; the freshness gate refuses) and must not be relied
///   on for anything else.
pub struct CmeGlobex {
    /// The exchange clock's offset from UTC in hours (CT = -6 standard,
    /// -5 daylight). Carried as data because Rust's std has no tz database;
    /// the caller who KNOWS the current offset supplies it.
    pub utc_offset_hours: i8,
}

impl CmeGlobex {
    pub fn standard() -> Self {
        Self { utc_offset_hours: -6 }
    }
    pub fn daylight() -> Self {
        Self { utc_offset_hours: -5 }
    }
}

impl Calendar for CmeGlobex {
    fn is_open(&self, ts: OffsetDateTime) -> bool {
        let local = ts + Duration::hours(self.utc_offset_hours as i64);
        let (wd, hour) = (local.weekday(), local.hour());
        match wd {
            Weekday::Saturday => false,
            Weekday::Sunday => hour >= 17,
            Weekday::Friday => hour < 16,
            _ => hour != 16, // the daily 16:00-17:00 CT maintenance halt
        }
    }
    fn name(&self) -> &'static str {
        "cme-globex"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn crypto_never_closes() {
        assert!(AlwaysOpen.is_open(datetime!(2026-01-01 00:00 UTC)));
    }

    #[test]
    fn cme_weekend_and_maintenance() {
        let cal = CmeGlobex::standard(); // CT = UTC-6
        // Saturday: closed all day.
        assert!(!cal.is_open(datetime!(2026-01-10 15:00 UTC)));
        // Sunday 17:30 CT = 23:30 UTC: open.
        assert!(cal.is_open(datetime!(2026-01-11 23:30 UTC)));
        // Wednesday 16:30 CT = 22:30 UTC: the maintenance halt.
        assert!(!cal.is_open(datetime!(2026-01-14 22:30 UTC)));
        // Wednesday 17:30 CT = 23:30 UTC: reopened.
        assert!(cal.is_open(datetime!(2026-01-14 23:30 UTC)));
        // Friday 16:30 CT = 22:30 UTC: closed for the week.
        assert!(!cal.is_open(datetime!(2026-01-16 22:30 UTC)));
    }
}
