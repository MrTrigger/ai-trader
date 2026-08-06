//! The DB-first source of truth: WHO is trading (identity registries,
//! step 1) and WHAT happened (operational records, step 4).
//!
//! Operational rows carry the bot's own document verbatim as jsonb text
//! next to a few indexed columns; this crate moves JSON as *strings* and
//! never parses it — the document dialects belong to the bots and the
//! dashboard, not to storage. Nothing in this crate touches credentials —
//! accounts carry a `credential_ref` naming the SOPS/env entry, and
//! resolution stays where it is today.
//!
//! Fail-closed contract: when `DATABASE_URL` is configured, an unregistered
//! or disabled bot must NOT run. When it is not configured (dev mode), the
//! caller may proceed file-only but must say so loudly.

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

#[derive(Debug, thiserror::Error)]
pub enum RecordsError {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("bot {0:?} is not registered — register it before running (fail closed)")]
    UnknownBot(String),
    #[error("bot {0:?} is registered but disabled — enable it before running (fail closed)")]
    DisabledBot(String),
}

#[derive(Debug, Clone)]
pub struct BotRow {
    pub bot_id: String,
    pub display_name: String,
    pub cadence: String,
    pub asset_class: String,
    pub decision_core: String,
    pub enabled: bool,
    /// Where the bot publishes state (repo-relative), until step 4 moves
    /// operational records into Postgres proper.
    pub state_dir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub bot_id: String,
    pub account_id: String,
    pub venue_id: String,
    pub scope: String,
    pub credential_ref: String,
    pub account_kind: String,
}

pub struct Registry {
    pool: PgPool,
}

impl Registry {
    pub async fn connect(database_url: &str) -> Result<Self, RecordsError> {
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), RecordsError> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    /// The fail-closed gate: the bot must exist AND be enabled.
    pub async fn require_enabled(&self, bot_id: &str) -> Result<BotRow, RecordsError> {
        let bot = self.bot(bot_id).await?;
        match bot {
            None => Err(RecordsError::UnknownBot(bot_id.into())),
            Some(b) if !b.enabled => Err(RecordsError::DisabledBot(bot_id.into())),
            Some(b) => Ok(b),
        }
    }

    pub async fn bot(&self, bot_id: &str) -> Result<Option<BotRow>, RecordsError> {
        let row = sqlx::query(
            "SELECT bot_id, display_name, cadence, asset_class, decision_core, enabled, state_dir \
             FROM bots WHERE bot_id = $1",
        )
        .bind(bot_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| BotRow {
            bot_id: r.get(0),
            display_name: r.get(1),
            cadence: r.get(2),
            asset_class: r.get(3),
            decision_core: r.get(4),
            enabled: r.get(5),
            state_dir: r.get(6),
        }))
    }

    pub async fn list_bots(&self) -> Result<Vec<BotRow>, RecordsError> {
        let rows = sqlx::query(
            "SELECT bot_id, display_name, cadence, asset_class, decision_core, enabled, state_dir \
             FROM bots ORDER BY bot_id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| BotRow {
                bot_id: r.get(0),
                display_name: r.get(1),
                cadence: r.get(2),
                asset_class: r.get(3),
                decision_core: r.get(4),
                enabled: r.get(5),
                state_dir: r.get(6),
            })
            .collect())
    }

    pub async fn bindings(&self, bot_id: &str) -> Result<Vec<Binding>, RecordsError> {
        let rows = sqlx::query(
            "SELECT b.bot_id, b.account_id, a.venue_id, b.scope, a.credential_ref, a.kind \
             FROM account_bindings b JOIN accounts a USING (account_id) \
             WHERE b.bot_id = $1 ORDER BY b.account_id",
        )
        .bind(bot_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Binding {
                bot_id: r.get(0),
                account_id: r.get(1),
                venue_id: r.get(2),
                scope: r.get(3),
                credential_ref: r.get(4),
                account_kind: r.get(5),
            })
            .collect())
    }

    /// Operator convenience: register (or update) a bot row. Registration
    /// never enables — enabling is a separate, deliberate act.
    pub async fn register_bot(
        &self,
        bot_id: &str,
        display_name: &str,
        cadence: &str,
        asset_class: &str,
        decision_core: &str,
    ) -> Result<(), RecordsError> {
        sqlx::query(
            "INSERT INTO bots (bot_id, display_name, cadence, asset_class, decision_core) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (bot_id) DO UPDATE SET display_name = $2, cadence = $3, \
             asset_class = $4, decision_core = $5",
        )
        .bind(bot_id)
        .bind(display_name)
        .bind(cadence)
        .bind(asset_class)
        .bind(decision_core)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_enabled(&self, bot_id: &str, enabled: bool) -> Result<(), RecordsError> {
        let res = sqlx::query("UPDATE bots SET enabled = $2 WHERE bot_id = $1")
            .bind(bot_id)
            .bind(enabled)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(RecordsError::UnknownBot(bot_id.into()));
        }
        Ok(())
    }
}

/// The current control word for a bot, i.e. the newest `control_events` row.
/// `payload` is the full control document in the bot's own dialect.
#[derive(Debug, Clone)]
pub struct ControlRow {
    pub state: String,
    pub reason: String,
    pub set_by: String,
    pub at: String,
    pub payload: String,
}

/// A bot's published state document plus its heartbeat.
#[derive(Debug, Clone)]
pub struct StatusRow {
    pub bot_id: String,
    pub heartbeat_at: String,
    /// Computed by Postgres at read time, so no clock-format parsing on the
    /// reader's side ever decides whether a bot looks alive.
    pub heartbeat_age_seconds: i64,
    pub payload: String,
}

// Operational records (step 4). Timestamps cross this boundary as RFC 3339
// text and are cast to timestamptz in SQL; JSON documents cross as text and
// are cast to jsonb — so Rust and Python callers speak the identical dialect.
impl Registry {
    /// Idempotent: re-recording the same (bot, run) overwrites — a crash
    /// between execute and record, then a retry, must not fail on conflict.
    pub async fn record_run(
        &self,
        bot_id: &str,
        run_id: &str,
        recorded_at: &str,
        outcome: &str,
        payload_json: &str,
    ) -> Result<(), RecordsError> {
        sqlx::query(
            "INSERT INTO runs (bot_id, run_id, recorded_at, outcome, payload) \
             VALUES ($1, $2, $3::timestamptz, $4, $5::jsonb) \
             ON CONFLICT (bot_id, run_id) DO UPDATE SET \
             recorded_at = EXCLUDED.recorded_at, outcome = EXCLUDED.outcome, \
             payload = EXCLUDED.payload",
        )
        .bind(bot_id)
        .bind(run_id)
        .bind(recorded_at)
        .bind(outcome)
        .bind(payload_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Most recent first, as JSON text per row.
    pub async fn recent_runs(
        &self,
        bot_id: &str,
        limit: i64,
    ) -> Result<Vec<String>, RecordsError> {
        let rows = sqlx::query(
            "SELECT payload::text FROM runs WHERE bot_id = $1 \
             ORDER BY recorded_at DESC LIMIT $2",
        )
        .bind(bot_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }

    /// When this plan last put orders on the wire, if it ever did. Mirrors
    /// the file store's guard: only `orders_submitted > 0` counts — a run
    /// that halted before sending anything must leave the plan runnable.
    pub async fn run_executed_at(
        &self,
        bot_id: &str,
        plan_id: &str,
    ) -> Result<Option<String>, RecordsError> {
        let row = sqlx::query(
            "SELECT payload->>'recorded_at' FROM runs \
             WHERE bot_id = $1 AND payload->>'plan_id' = $2 \
             AND COALESCE((payload->>'orders_submitted')::int, 0) > 0 \
             ORDER BY recorded_at DESC LIMIT 1",
        )
        .bind(bot_id)
        .bind(plan_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get(0)))
    }

    pub async fn ledger_append(
        &self,
        bot_id: &str,
        at: &str,
        kind: &str,
        client_order_id: Option<&str>,
        payload_json: &str,
    ) -> Result<(), RecordsError> {
        sqlx::query(
            "INSERT INTO ledger_entries (bot_id, at, kind, client_order_id, payload) \
             VALUES ($1, $2::timestamptz, $3, $4, $5::jsonb)",
        )
        .bind(bot_id)
        .bind(at)
        .bind(kind)
        .bind(client_order_id)
        .bind(payload_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every entry, in the exact order written.
    pub async fn ledger_entries(&self, bot_id: &str) -> Result<Vec<String>, RecordsError> {
        let rows = sqlx::query(
            "SELECT payload::text FROM ledger_entries WHERE bot_id = $1 ORDER BY seq",
        )
        .bind(bot_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }

    /// Idempotent on (bot, fill_key): replays and recovery reruns re-derive
    /// the same key and the insert becomes a no-op.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_fill(
        &self,
        bot_id: &str,
        fill_key: &str,
        at: &str,
        instrument: Option<&str>,
        sleeve: Option<&str>,
        side: Option<&str>,
        qty: Option<&str>,
        price: Option<&str>,
        pnl: Option<&str>,
        reason: Option<&str>,
        payload_json: &str,
    ) -> Result<(), RecordsError> {
        sqlx::query(
            "INSERT INTO fills (bot_id, fill_key, at, instrument, sleeve, side, \
             qty, price, pnl, reason, payload) \
             VALUES ($1, $2, $3::timestamptz, $4, $5, $6, $7::numeric, \
             $8::numeric, $9::numeric, $10, $11::jsonb) \
             ON CONFLICT (bot_id, fill_key) DO NOTHING",
        )
        .bind(bot_id)
        .bind(fill_key)
        .bind(at)
        .bind(instrument)
        .bind(sleeve)
        .bind(side)
        .bind(qty)
        .bind(price)
        .bind(pnl)
        .bind(reason)
        .bind(payload_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A fresh (non-resume) replay owns its history: the strategy that
    /// produced the old rows may no longer be the strategy running.
    pub async fn clear_fills(&self, bot_id: &str) -> Result<(), RecordsError> {
        sqlx::query("DELETE FROM fills WHERE bot_id = $1")
            .bind(bot_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Most recent first, as JSON text per row.
    pub async fn recent_fills(
        &self,
        bot_id: &str,
        limit: i64,
    ) -> Result<Vec<String>, RecordsError> {
        let rows = sqlx::query(
            "SELECT payload::text FROM fills WHERE bot_id = $1 \
             ORDER BY at DESC, fill_key DESC LIMIT $2",
        )
        .bind(bot_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }

    /// Append a control word. Absence of any row means HALTED (fail closed);
    /// this never updates in place — controls are an audit trail.
    pub async fn set_control(
        &self,
        bot_id: &str,
        state: &str,
        reason: &str,
        set_by: &str,
        payload_json: &str,
    ) -> Result<(), RecordsError> {
        sqlx::query(
            "INSERT INTO control_events (bot_id, state, reason, set_by, payload) \
             VALUES ($1, $2, $3, $4, $5::jsonb)",
        )
        .bind(bot_id)
        .bind(state)
        .bind(reason)
        .bind(set_by)
        .bind(payload_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn current_control(
        &self,
        bot_id: &str,
    ) -> Result<Option<ControlRow>, RecordsError> {
        let row = sqlx::query(
            "SELECT state, reason, set_by, at::text, payload::text \
             FROM control_events WHERE bot_id = $1 ORDER BY seq DESC LIMIT 1",
        )
        .bind(bot_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| ControlRow {
            state: r.get(0),
            reason: r.get(1),
            set_by: r.get(2),
            at: r.get(3),
            payload: r.get(4),
        }))
    }

    pub async fn put_status(
        &self,
        bot_id: &str,
        heartbeat_at: &str,
        payload_json: &str,
    ) -> Result<(), RecordsError> {
        sqlx::query(
            "INSERT INTO bot_status (bot_id, heartbeat_at, payload) \
             VALUES ($1, $2::timestamptz, $3::jsonb) \
             ON CONFLICT (bot_id) DO UPDATE SET \
             heartbeat_at = EXCLUDED.heartbeat_at, payload = EXCLUDED.payload",
        )
        .bind(bot_id)
        .bind(heartbeat_at)
        .bind(payload_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_status(&self, bot_id: &str) -> Result<Option<StatusRow>, RecordsError> {
        let row = sqlx::query(
            "SELECT bot_id, heartbeat_at::text, \
             EXTRACT(EPOCH FROM now() - heartbeat_at)::bigint, payload::text \
             FROM bot_status WHERE bot_id = $1",
        )
        .bind(bot_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| StatusRow {
            bot_id: r.get(0),
            heartbeat_at: r.get(1),
            heartbeat_age_seconds: r.get(2),
            payload: r.get(3),
        }))
    }

    pub async fn all_status(&self) -> Result<Vec<StatusRow>, RecordsError> {
        let rows = sqlx::query(
            "SELECT bot_id, heartbeat_at::text, \
             EXTRACT(EPOCH FROM now() - heartbeat_at)::bigint, payload::text \
             FROM bot_status ORDER BY bot_id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| StatusRow {
                bot_id: r.get(0),
                heartbeat_at: r.get(1),
                heartbeat_age_seconds: r.get(2),
                payload: r.get(3),
            })
            .collect())
    }

    pub async fn put_snapshot(
        &self,
        bot_id: &str,
        taken_at: &str,
        payload_json: &str,
    ) -> Result<(), RecordsError> {
        sqlx::query(
            "INSERT INTO snapshots (bot_id, taken_at, payload) \
             VALUES ($1, $2::timestamptz, $3::jsonb) \
             ON CONFLICT (bot_id) DO UPDATE SET \
             taken_at = EXCLUDED.taken_at, payload = EXCLUDED.payload",
        )
        .bind(bot_id)
        .bind(taken_at)
        .bind(payload_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_snapshot(&self, bot_id: &str) -> Result<Option<String>, RecordsError> {
        let row = sqlx::query("SELECT payload::text FROM snapshots WHERE bot_id = $1")
            .bind(bot_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get(0)))
    }

    pub async fn put_sim_state(
        &self,
        bot_id: &str,
        updated_at: &str,
        payload_json: &str,
    ) -> Result<(), RecordsError> {
        sqlx::query(
            "INSERT INTO venue_sim_state (bot_id, updated_at, payload) \
             VALUES ($1, $2::timestamptz, $3::jsonb) \
             ON CONFLICT (bot_id) DO UPDATE SET \
             updated_at = EXCLUDED.updated_at, payload = EXCLUDED.payload",
        )
        .bind(bot_id)
        .bind(updated_at)
        .bind(payload_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_sim_state(&self, bot_id: &str) -> Result<Option<String>, RecordsError> {
        let row = sqlx::query("SELECT payload::text FROM venue_sim_state WHERE bot_id = $1")
            .bind(bot_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get(0)))
    }
}

/// Synchronous facade for callers that are not async (the runner's money
/// path). One current-thread runtime, one pool, shared behind an `Arc`.
pub mod blocking {
    use std::sync::Arc;

    use super::{ControlRow, RecordsError, Registry, StatusRow};

    pub struct Records {
        rt: tokio::runtime::Runtime,
        inner: Registry,
    }

    impl Records {
        pub fn connect(database_url: &str) -> Result<Arc<Self>, RecordsError> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio current-thread runtime");
            let inner = wait_on(&rt, Registry::connect(database_url))?;
            Ok(Arc::new(Self { rt, inner }))
        }

        pub fn registry(&self) -> &Registry {
            &self.inner
        }

        fn wait<F>(&self, fut: F) -> F::Output
        where
            F: std::future::Future + Send,
            F::Output: Send,
        {
            wait_on(&self.rt, fut)
        }

        pub fn record_run(
            &self,
            bot_id: &str,
            run_id: &str,
            recorded_at: &str,
            outcome: &str,
            payload_json: &str,
        ) -> Result<(), RecordsError> {
            self.wait(
                self.inner
                    .record_run(bot_id, run_id, recorded_at, outcome, payload_json),
            )
        }

        pub fn recent_runs(&self, bot_id: &str, limit: i64) -> Result<Vec<String>, RecordsError> {
            self.wait(self.inner.recent_runs(bot_id, limit))
        }

        pub fn run_executed_at(
            &self,
            bot_id: &str,
            plan_id: &str,
        ) -> Result<Option<String>, RecordsError> {
            self.wait(self.inner.run_executed_at(bot_id, plan_id))
        }

        pub fn ledger_append(
            &self,
            bot_id: &str,
            at: &str,
            kind: &str,
            client_order_id: Option<&str>,
            payload_json: &str,
        ) -> Result<(), RecordsError> {
            self.wait(
                self.inner
                    .ledger_append(bot_id, at, kind, client_order_id, payload_json),
            )
        }

        pub fn ledger_entries(&self, bot_id: &str) -> Result<Vec<String>, RecordsError> {
            self.wait(self.inner.ledger_entries(bot_id))
        }

        pub fn set_control(
            &self,
            bot_id: &str,
            state: &str,
            reason: &str,
            set_by: &str,
            payload_json: &str,
        ) -> Result<(), RecordsError> {
            self.wait(self.inner.set_control(bot_id, state, reason, set_by, payload_json))
        }

        pub fn current_control(&self, bot_id: &str) -> Result<Option<ControlRow>, RecordsError> {
            self.wait(self.inner.current_control(bot_id))
        }

        pub fn put_status(
            &self,
            bot_id: &str,
            heartbeat_at: &str,
            payload_json: &str,
        ) -> Result<(), RecordsError> {
            self.wait(self.inner.put_status(bot_id, heartbeat_at, payload_json))
        }

        #[allow(clippy::too_many_arguments)]
        pub fn record_fill(
            &self,
            bot_id: &str,
            fill_key: &str,
            at: &str,
            instrument: Option<&str>,
            sleeve: Option<&str>,
            side: Option<&str>,
            qty: Option<&str>,
            price: Option<&str>,
            pnl: Option<&str>,
            reason: Option<&str>,
            payload_json: &str,
        ) -> Result<(), RecordsError> {
            self.wait(self.inner.record_fill(
                bot_id, fill_key, at, instrument, sleeve, side, qty, price, pnl, reason,
                payload_json,
            ))
        }

        pub fn clear_fills(&self, bot_id: &str) -> Result<(), RecordsError> {
            self.wait(self.inner.clear_fills(bot_id))
        }

        pub fn put_snapshot(
            &self,
            bot_id: &str,
            taken_at: &str,
            payload_json: &str,
        ) -> Result<(), RecordsError> {
            self.wait(self.inner.put_snapshot(bot_id, taken_at, payload_json))
        }

        pub fn get_snapshot(&self, bot_id: &str) -> Result<Option<String>, RecordsError> {
            self.wait(self.inner.get_snapshot(bot_id))
        }

        pub fn recent_fills(&self, bot_id: &str, limit: i64) -> Result<Vec<String>, RecordsError> {
            self.wait(self.inner.recent_fills(bot_id, limit))
        }

        pub fn get_status(&self, bot_id: &str) -> Result<Option<StatusRow>, RecordsError> {
            self.wait(self.inner.get_status(bot_id))
        }

        pub fn put_sim_state(
            &self,
            bot_id: &str,
            updated_at: &str,
            payload_json: &str,
        ) -> Result<(), RecordsError> {
            self.wait(self.inner.put_sim_state(bot_id, updated_at, payload_json))
        }

        pub fn get_sim_state(&self, bot_id: &str) -> Result<Option<String>, RecordsError> {
            self.wait(self.inner.get_sim_state(bot_id))
        }
    }

    /// Drive a future to completion from a plain thread, even when the caller
    /// is already inside some other tokio runtime. `block_on` on the calling
    /// thread would panic there; a scoped thread has no runtime context, so
    /// it is always legal — and the caller genuinely wants to block.
    fn wait_on<F>(rt: &tokio::runtime::Runtime, fut: F) -> F::Output
    where
        F: std::future::Future + Send,
        F::Output: Send,
    {
        std::thread::scope(|s| {
            s.spawn(|| rt.block_on(fut))
                .join()
                .expect("records db call panicked")
        })
    }
}
