//! The dashboard page.
//!
//! One file, no build step, no external requests — the same discipline as the
//! research viewer. A page that halts a trading system should not depend on a
//! CDN being reachable, and the operator who most needs it is the one whose
//! network is already having a bad day.

pub const HTML: &str = include_str!("dashboard.html");
