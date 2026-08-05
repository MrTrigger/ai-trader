//! Our own record of what we authorised, kept apart from the venue's.
//!
//! # Why this exists at all
//!
//! Reconciliation is only worth running if the two sides are independent. Until
//! now "our" positions were folded from `venue.get_fills()` — the venue's record
//! on both sides of the comparison, which can never disagree with itself. The
//! `paper` crate says as much in its own docs. Against a real venue that is not
//! a harmless simplification: it means the check that exists to catch a
//! compromised key, a stale resting order from a previous deployment, or a
//! second process trading the same account would pass every time.
//!
//! So the bot keeps an append-only ledger of the order ids it authorised. A
//! venue fill carrying an id we never issued is then visible, and it is the
//! single most alarming thing this system can observe.
//!
//! # Reconnecting to an account that already holds positions
//!
//! A fresh install, a lost ledger, or the first connection to a funded account
//! all look the same: the venue holds positions and we have no record of them.
//! That must not be silently absorbed — adopting whatever the venue says is
//! exactly the auto-repair the executor refuses to do — and it must not be a
//! permanent halt either, or the system could never be connected to a live
//! account in the first place.
//!
//! The answer is [`Entry::Baseline`]: an opening position, written once,
//! deliberately, by a named human who has looked at the account. After that the
//! ledger explains the book and reconciliation has teeth again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use venue::{Fill, Position};

use crate::{stamp, RunnerError};

/// One line of the ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// A position that already existed when we connected, adopted on purpose.
    ///
    /// Carries who and why, because this is the one entry that asserts
    /// something we did not do ourselves.
    Baseline {
        asset: String,
        #[serde(with = "rust_decimal::serde::str")]
        qty: Decimal,
        #[serde(with = "rust_decimal::serde::str")]
        avg_price: Decimal,
        at: String,
        by: String,
        reason: String,
    },
    /// An order we are about to send.
    ///
    /// Written **before** submission, not after. A crash between the write and
    /// the send leaves an authorised id that never filled, which is harmless.
    /// The other order leaves a fill we cannot account for, which reads as an
    /// intrusion and halts the system.
    Submitted {
        client_order_id: String,
        asset: String,
        side: String,
        #[serde(with = "rust_decimal::serde::str")]
        qty: Decimal,
        at: String,
        plan_id: String,
        slice: u8,
    },
    /// A venue fill we did not authorise, that a human has looked at and
    /// decided to stop being alarmed by.
    ///
    /// It exists because the alternative is a dead end. A lost ledger against a
    /// venue that still reports the old fills is indistinguishable, to the
    /// system, from someone else trading the account — and without a way to say
    /// "I have checked, this was me", the only remaining state is halted
    /// forever.
    ///
    /// Acknowledging does **not** fold the fill into our position; the
    /// accompanying baseline already accounts for where the book ended up.
    /// Every field of the fill is copied here so the evidence survives even if
    /// the venue stops reporting it.
    Acknowledged {
        venue_fill_id: String,
        client_order_id: String,
        asset: String,
        #[serde(with = "rust_decimal::serde::str")]
        qty: Decimal,
        fill_at: String,
        at: String,
        by: String,
        reason: String,
    },
}

/// What our record says, next to what the venue says.
#[derive(Debug, Clone, Serialize)]
pub struct Reconciliation {
    pub ours: Vec<PositionLine>,
    pub theirs: Vec<PositionLine>,
    /// Assets where the two disagree by more than the epsilon.
    pub disagreements: Vec<Disagreement>,
    /// Venue fills carrying ids we never authorised. Always empty, or an alarm.
    pub unknown_fills: Vec<UnknownFill>,
    /// The venue holds these and our ledger explains none of them.
    pub unadopted: Vec<String>,
    pub ledger_entries: usize,
    pub agrees: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionLine {
    pub asset: String,
    pub qty: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Disagreement {
    pub asset: String,
    pub ours: String,
    pub theirs: String,
    pub difference: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnknownFill {
    pub venue_fill_id: String,
    pub client_order_id: String,
    pub asset: String,
    pub qty: String,
    pub at: String,
}

/// An append-only file of [`Entry`], one JSON object per line.
///
/// JSONL rather than one JSON document: appending is a single write with no
/// read-modify-write, so two processes cannot lose each other's entries, and a
/// truncated last line costs one entry rather than the whole file.
pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    pub fn open(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join("ledger.jsonl"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, entry: &Entry) -> Result<(), RunnerError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| RunnerError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let line = serde_json::to_string(entry).expect("ledger entry serialises");
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| RunnerError::Io {
                path: self.path.clone(),
                source,
            })?;
        writeln!(f, "{line}").map_err(|source| RunnerError::Io {
            path: self.path.clone(),
            source,
        })
    }

    pub fn read(&self) -> Result<Vec<Entry>, RunnerError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(RunnerError::Io {
                    path: self.path.clone(),
                    source,
                })
            }
        };
        text.lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| {
                serde_json::from_str(l).map_err(|e| RunnerError::Malformed {
                    what: "ledger entry",
                    path: self.path.clone(),
                    detail: format!("line {}: {e}", i + 1),
                })
            })
            .collect()
    }

    pub fn is_empty(&self) -> Result<bool, RunnerError> {
        Ok(self.read()?.is_empty())
    }

    /// Record an order before it is sent. See [`Entry::Submitted`].
    ///
    /// Takes the order itself rather than its fields spread out: the id must
    /// describe the order that is actually going to the venue, and six loose
    /// parameters is how those two drift apart.
    pub fn authorise(
        &self,
        client_order_id: &str,
        order: &plan::Order,
        plan_id: &str,
        slice: u8,
        now: OffsetDateTime,
    ) -> Result<(), RunnerError> {
        self.append(&Entry::Submitted {
            client_order_id: client_order_id.to_string(),
            asset: order.asset.clone(),
            side: format!("{:?}", order.side).to_lowercase(),
            qty: order.qty,
            at: stamp(now),
            plan_id: plan_id.to_string(),
            slice,
        })
    }

    /// Adopt the venue's current positions as our opening state.
    ///
    /// Returns the entries written. Refuses nothing on its own — the caller
    /// decides whether adopting is appropriate, because only a human can know
    /// whether an unexplained position is a legacy holding or a problem.
    pub fn adopt(
        &self,
        positions: &[Position],
        by: &str,
        reason: &str,
        now: OffsetDateTime,
    ) -> Result<Vec<Entry>, RunnerError> {
        let mut written = Vec::new();
        for p in positions.iter().filter(|p| !p.qty.is_zero()) {
            let e = Entry::Baseline {
                asset: p.asset.clone(),
                qty: p.qty,
                avg_price: p.avg_price,
                at: stamp(now),
                by: by.to_string(),
                reason: reason.to_string(),
            };
            self.append(&e)?;
            written.push(e);
        }
        Ok(written)
    }

    /// Our view of the book: adopted baselines, plus the venue's fills for
    /// orders we authorised.
    ///
    /// This is what the executor reconciles against. It deliberately ignores
    /// fills we did not authorise — folding those in would make our record
    /// agree with the venue by construction, which is the failure this whole
    /// module exists to remove.
    pub fn our_positions(&self, venue_fills: &[Fill]) -> Result<Vec<Position>, RunnerError> {
        let entries = self.read()?;
        let authorised: BTreeSet<&str> = entries
            .iter()
            .filter_map(|e| match e {
                Entry::Submitted {
                    client_order_id, ..
                } => Some(client_order_id.as_str()),
                _ => None,
            })
            .collect();

        let mut qty: BTreeMap<String, Decimal> = BTreeMap::new();
        let mut cost: BTreeMap<String, Decimal> = BTreeMap::new();
        for e in &entries {
            if let Entry::Baseline {
                asset,
                qty: q,
                avg_price,
                ..
            } = e
            {
                *qty.entry(asset.clone()).or_default() += *q;
                *cost.entry(asset.clone()).or_default() += *q * *avg_price;
            }
        }
        let acknowledged: BTreeSet<&str> = entries
            .iter()
            .filter_map(|e| match e {
                Entry::Acknowledged { venue_fill_id, .. } => Some(venue_fill_id.as_str()),
                _ => None,
            })
            .collect();
        for f in venue_fills.iter().filter(|f| {
            authorised.contains(f.client_order_id.as_str())
                && !acknowledged.contains(f.venue_fill_id.as_str())
        }) {
            *qty.entry(f.asset.clone()).or_default() += f.signed_qty();
            *cost.entry(f.asset.clone()).or_default() += f.signed_qty() * f.price;
        }

        Ok(qty
            .into_iter()
            .filter(|(_, q)| !q.is_zero())
            .map(|(asset, q)| {
                let c = cost.get(&asset).copied().unwrap_or_default();
                Position {
                    avg_price: if q.is_zero() { Decimal::ZERO } else { c / q },
                    asset,
                    qty: q,
                }
            })
            .collect())
    }

    /// Compare our record against the venue's, without changing either.
    ///
    /// `epsilon` is the tolerance from the executor: venues round to their own
    /// lot sizes, so exact equality would flag arithmetic rather than drift.
    pub fn reconcile(
        &self,
        venue_positions: &[Position],
        venue_fills: &[Fill],
        epsilon: Decimal,
    ) -> Result<Reconciliation, RunnerError> {
        let entries = self.read()?;
        let mut authorised: BTreeSet<&str> = BTreeSet::new();
        let mut acknowledged: BTreeSet<&str> = BTreeSet::new();
        let mut ours: BTreeMap<String, Decimal> = BTreeMap::new();
        let mut has_baseline: BTreeSet<&str> = BTreeSet::new();

        for e in &entries {
            match e {
                Entry::Baseline { asset, qty, .. } => {
                    *ours.entry(asset.clone()).or_default() += *qty;
                    has_baseline.insert(asset);
                }
                Entry::Submitted {
                    client_order_id, ..
                } => {
                    authorised.insert(client_order_id);
                }
                Entry::Acknowledged { venue_fill_id, .. } => {
                    acknowledged.insert(venue_fill_id);
                }
            }
        }

        // Our position moves on the venue's fills, but only for orders we
        // authorised. Anything else is not ours to fold in.
        let mut unknown_fills = Vec::new();
        for f in venue_fills {
            // An acknowledged fill is neither ours to fold in nor an alarm any
            // more: a human has looked at it, and the baseline written at the
            // same time already accounts for the position it produced.
            if acknowledged.contains(f.venue_fill_id.as_str()) {
                continue;
            }
            if authorised.contains(f.client_order_id.as_str()) {
                *ours.entry(f.asset.clone()).or_default() += f.signed_qty();
            } else {
                unknown_fills.push(UnknownFill {
                    venue_fill_id: f.venue_fill_id.clone(),
                    client_order_id: f.client_order_id.clone(),
                    asset: f.asset.clone(),
                    qty: f.qty.normalize().to_string(),
                    at: stamp(f.ts),
                });
            }
        }

        let theirs: BTreeMap<&str, Decimal> = venue_positions
            .iter()
            .map(|p| (p.asset.as_str(), p.qty))
            .collect();

        let assets: BTreeSet<&str> = ours
            .keys()
            .map(|s| s.as_str())
            .chain(theirs.keys().copied())
            .collect();

        let mut disagreements = Vec::new();
        let mut unadopted = Vec::new();
        for asset in &assets {
            let a = ours.get(*asset).copied().unwrap_or_default();
            let b = theirs.get(*asset).copied().unwrap_or_default();
            if (a - b).abs() <= epsilon {
                continue;
            }
            disagreements.push(Disagreement {
                asset: (*asset).to_string(),
                ours: a.normalize().to_string(),
                theirs: b.normalize().to_string(),
                difference: (b - a).normalize().to_string(),
            });
            // The specific, recoverable case: the venue holds something and we
            // have no record of it at all. Distinguished from general drift
            // because the remedy is different and actionable.
            if a.is_zero() && !b.is_zero() && !has_baseline.contains(*asset) {
                unadopted.push((*asset).to_string());
            }
        }

        Ok(Reconciliation {
            ours: ours
                .iter()
                .filter(|(_, q)| !q.is_zero())
                .map(|(a, q)| PositionLine {
                    asset: a.clone(),
                    qty: q.normalize().to_string(),
                })
                .collect(),
            // Sorted the same way as `ours`, because the two are printed next
            // to each other and a human comparing them should not have to
            // match up rows first.
            theirs: theirs
                .iter()
                .filter(|(_, q)| !q.is_zero())
                .map(|(a, q)| PositionLine {
                    asset: (*a).to_string(),
                    qty: q.normalize().to_string(),
                })
                .collect(),
            agrees: disagreements.is_empty() && unknown_fills.is_empty(),
            disagreements,
            unknown_fills,
            unadopted,
            ledger_entries: entries.len(),
        })
    }
}

impl Reconciliation {
    /// The sentence an operator needs, and the command that fixes it.
    ///
    /// Every branch names a remedy. "Reconciliation failed" with no next step
    /// is how a system ends up being restarted until it stops complaining.
    pub fn explain(&self) -> String {
        if !self.unknown_fills.is_empty() {
            let f = &self.unknown_fills[0];
            return format!(
                "the venue reports a fill we never authorised: {} of {} under order id {} at {}. \
                 Our ledger has no record of asking for it. This is the most serious thing this \
                 system can observe. It means one of: another process trading this account, a \
                 stale order from an earlier deployment, a compromised key - or, benignly, that \
                 this bot's own ledger was lost and the venue still remembers what it did. \
                 STOPPED, because those look identical from here and only you can tell them \
                 apart. If you have checked the account and it was us, \
                 `bot adopt --accept-unknown-fills` records that judgement against your name \
                 and moves on. ({} such fills in total.)",
                f.qty,
                f.asset,
                f.client_order_id,
                f.at,
                self.unknown_fills.len()
            );
        }
        if !self.unadopted.is_empty() {
            return format!(
                "the venue holds {} that our ledger does not explain: {}. That is what a fresh \
                 install, a lost ledger, or a first connection to a funded account looks like. \
                 Check the account, then run `bot adopt --reason ... --by you --confirm` to take \
                 these on as an opening position. Nothing is adopted automatically: absorbing \
                 whatever the venue says is exactly the repair that hides a bug.",
                if self.unadopted.len() == 1 {
                    "a position"
                } else {
                    "positions"
                },
                self.unadopted.join(", ")
            );
        }
        if !self.disagreements.is_empty() {
            let d = &self.disagreements[0];
            return format!(
                "our record and the venue disagree on {}: we derive {}, the venue reports {} \
                 (off by {}). One of our assumptions is wrong, and trading on top of it turns a \
                 bug into a loss. STOPPED - this is never auto-corrected.",
                d.asset, d.ours, d.theirs, d.difference
            );
        }
        format!(
            "our record and the venue agree on {} position(s), from {} ledger entries.",
            self.theirs.len(),
            self.ledger_entries
        )
    }
}
