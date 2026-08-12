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
    #[error("refused: {0}")]
    Refused(String),
    #[error(
        "bot {bot_id:?} still owns operational records ({held}). Deregistering would orphan \
         history somebody may need to audit. Disable it instead, or clear those rows first if \
         they were never real."
    )]
    BotHasRecords { bot_id: String, held: String },
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
    /// How the api launches this bot's process (repo-relative command).
    /// NULL: the api cannot start it and the dashboard says so.
    pub launch: Option<String>,
}

/// A venue account the fleet knows about. Credentials are NOT here — only
/// the reference naming the SOPS/env entry that holds them.
#[derive(Debug, Clone)]
pub struct AccountRow {
    pub account_id: String,
    /// WHO holds it (a company: `ib`, `amp`, `hyperliquid`).
    pub venue_id: String,
    /// HOW it is reached (`ib`, `rithmic`, `hyperliquid`): the adapter
    /// selector.
    pub protocol: String,
    /// `paper` or `live`.
    pub kind: String,
    pub credential_ref: String,
    /// What this venue can trade. A futures book must never be offered a
    /// crypto venue, so the control plane filters on this.
    pub asset_classes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub bot_id: String,
    pub account_id: String,
    /// WHO holds the account (a company: `ib`, `amp`).
    pub venue_id: String,
    /// HOW we reach them (`ib`, `rithmic`, `hyperliquid`). Adapters are
    /// selected by this, never by the venue's name — two brokers can speak
    /// the same protocol, and one broker could change protocol.
    pub protocol: String,
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
            "SELECT bot_id, display_name, cadence, asset_class, decision_core, enabled, state_dir, launch \
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
            launch: r.get(7),
        }))
    }

    pub async fn list_bots(&self) -> Result<Vec<BotRow>, RecordsError> {
        let rows = sqlx::query(
            "SELECT bot_id, display_name, cadence, asset_class, decision_core, enabled, state_dir, launch \
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
                launch: r.get(7),
            })
            .collect())
    }

    pub async fn bindings(&self, bot_id: &str) -> Result<Vec<Binding>, RecordsError> {
        let rows = sqlx::query(
            "SELECT b.bot_id, b.account_id, a.venue_id, b.scope, a.credential_ref, a.kind, \
             v.protocol \
             FROM account_bindings b JOIN accounts a USING (account_id) \
             JOIN venues v USING (venue_id) \
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
                protocol: r.get(6),
            })
            .collect())
    }

    pub async fn list_accounts(&self) -> Result<Vec<AccountRow>, RecordsError> {
        let rows = sqlx::query(
            "SELECT a.account_id, a.venue_id, a.kind, a.credential_ref, v.protocol, \
             v.asset_classes \
             FROM accounts a JOIN venues v USING (venue_id) \
             ORDER BY a.venue_id, a.kind, a.account_id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| AccountRow {
                account_id: r.get(0),
                venue_id: r.get(1),
                kind: r.get(2),
                credential_ref: r.get(3),
                protocol: r.get(4),
                asset_classes: r.get(5),
            })
            .collect())
    }

    /// Point a bot's TRADE scope at `account_id`. One trade account per bot,
    /// always: two would mean two books and one ledger.
    ///
    /// Transactional, because the intermediate state — a bot with no trade
    /// binding at all — is one a starting bot would read as "default venue"
    /// and act on.
    pub async fn set_trade_binding(
        &self,
        bot_id: &str,
        account_id: &str,
    ) -> Result<(), RecordsError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM account_bindings WHERE bot_id = $1 AND scope = 'trade' \
             AND account_id <> $2",
        )
        .bind(bot_id)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO account_bindings (bot_id, account_id, scope) VALUES ($1, $2, 'trade') \
             ON CONFLICT (bot_id, account_id) DO UPDATE SET scope = 'trade'",
        )
        .bind(bot_id)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Operator convenience: register (or update) a bot row. Registration
    /// never enables — enabling is a separate, deliberate act.
    ///
    /// `launch` is the shell command that starts one process for this bot, and
    /// it is deployment-specific: on a laptop it is a repo path, in the image
    /// it is an absolute one. So it arrives here from whoever is registering
    /// rather than from a migration, and a `None` keeps whatever is recorded.
    #[allow(clippy::too_many_arguments)]
    pub async fn register_bot(
        &self,
        bot_id: &str,
        display_name: &str,
        cadence: &str,
        asset_class: &str,
        decision_core: &str,
        state_dir: Option<&str>,
        launch: Option<&str>,
    ) -> Result<(), RecordsError> {
        // state_dir is how anything finds this bot's files before it has ever
        // run and written a status row. Left null, the fleet view falls all the
        // way through to `{"contract":"none"}` and the page renders a
        // registered, enabled bot as unbound and disabled - which is what a
        // freshly deployed bot looked like until this was recorded.
        sqlx::query(
            "INSERT INTO bots (bot_id, display_name, cadence, asset_class, decision_core, \
             state_dir, launch) VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (bot_id) DO UPDATE SET display_name = $2, cadence = $3, \
             asset_class = $4, decision_core = $5, \
             state_dir = COALESCE(EXCLUDED.state_dir, bots.state_dir), \
             launch = COALESCE(EXCLUDED.launch, bots.launch)",
        )
        .bind(bot_id)
        .bind(display_name)
        .bind(cadence)
        .bind(asset_class)
        .bind(decision_core)
        .bind(state_dir)
        .bind(launch)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Register (or update) an account at a venue that already exists.
    ///
    /// The venues are reference data and arrive by migration; the accounts on
    /// them are per-environment, because which paper account you hold is not a
    /// property of the code. They were hand-written SQL until a second
    /// environment needed the same rows and there was nothing to run.
    ///
    /// `credential_ref` names the `.env` block that holds the secrets — the
    /// reference travels in the database, the secret never does.
    pub async fn register_account(
        &self,
        account_id: &str,
        venue_id: &str,
        kind: &str,
        credential_ref: &str,
        quote_currency: &str,
        notes: Option<&str>,
    ) -> Result<(), RecordsError> {
        if kind != "paper" && kind != "live" {
            return Err(RecordsError::Refused(format!(
                "account kind {kind:?} is neither paper nor live"
            )));
        }
        sqlx::query(
            "INSERT INTO accounts (account_id, venue_id, kind, credential_ref, quote_currency, \
             notes) VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (account_id) DO UPDATE SET venue_id = $2, kind = $3, \
             credential_ref = $4, quote_currency = $5, \
             notes = COALESCE(EXCLUDED.notes, accounts.notes)",
        )
        .bind(account_id)
        .bind(venue_id)
        .bind(kind)
        .bind(credential_ref)
        .bind(quote_currency)
        .bind(notes)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// How much operational history a bot owns, across every records table.
    ///
    /// Asked before deregistering: a bot with fills is a bot whose history
    /// someone will want to audit, and the identity row is what makes those
    /// rows attributable.
    pub async fn record_counts(
        &self,
        bot_id: &str,
    ) -> Result<Vec<(&'static str, i64)>, RecordsError> {
        let mut out = Vec::new();
        for table in [
            "bot_status",
            "control_events",
            "fills",
            "runs",
            "ledger_entries",
            "snapshots",
            "venue_sim_state",
            "account_bindings",
        ] {
            // Table names come from this fixed list, never from a caller.
            let sql = format!("SELECT count(*) FROM {table} WHERE bot_id = $1");
            let n: i64 = sqlx::query_scalar(&sql)
                .bind(bot_id)
                .fetch_one(&self.pool)
                .await?;
            if n > 0 {
                out.push((table, n));
            }
        }
        Ok(out)
    }

    /// Deregister a bot. Refuses while it still owns operational records —
    /// the foreign keys would refuse anyway, and an error naming the tables is
    /// more use than one naming a constraint.
    pub async fn remove_bot(&self, bot_id: &str) -> Result<(), RecordsError> {
        let held = self.record_counts(bot_id).await?;
        if !held.is_empty() {
            return Err(RecordsError::BotHasRecords {
                bot_id: bot_id.into(),
                held: held
                    .iter()
                    .map(|(t, n)| format!("{n} in {t}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
        let res = sqlx::query("DELETE FROM bots WHERE bot_id = $1")
            .bind(bot_id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(RecordsError::UnknownBot(bot_id.into()));
        }
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

    /// Keep the decision, not just its consequence.
    ///
    /// Idempotent on `(bot_id, plan_id)`: a plan re-recorded is the same plan,
    /// because `plan_id` is a content hash. `run_id` is filled in when the plan
    /// is executed and stays null when it is refused — a refused plan is worth
    /// keeping precisely because it was refused.
    pub async fn record_plan(
        &self,
        bot_id: &str,
        plan_id: &str,
        run_id: Option<&str>,
        as_of: &str,
        created_at: &str,
        payload_json: &str,
    ) -> Result<(), RecordsError> {
        sqlx::query(
            "INSERT INTO plans (bot_id, plan_id, run_id, as_of, created_at, payload) \
             VALUES ($1, $2, $3, $4::timestamptz, $5::timestamptz, $6::jsonb) \
             ON CONFLICT (bot_id, plan_id) DO UPDATE SET \
             run_id = COALESCE(EXCLUDED.run_id, plans.run_id), \
             payload = EXCLUDED.payload",
        )
        .bind(bot_id)
        .bind(plan_id)
        .bind(run_id)
        .bind(as_of)
        .bind(created_at)
        .bind(payload_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The plan behind one run, if it was kept. Deliberately by run rather than
    /// by plan id: the question is always "what did THAT decision see".
    pub async fn plan_for_run(
        &self,
        bot_id: &str,
        run_id: &str,
    ) -> Result<Option<String>, RecordsError> {
        let row = sqlx::query(
            "SELECT payload::text FROM plans WHERE bot_id = $1 AND run_id = $2 LIMIT 1",
        )
        .bind(bot_id)
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get(0)))
    }

    /// Most recent first, as JSON text per row.
    pub async fn recent_runs(&self, bot_id: &str, limit: i64) -> Result<Vec<String>, RecordsError> {
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
        let rows =
            sqlx::query("SELECT payload::text FROM ledger_entries WHERE bot_id = $1 ORDER BY seq")
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

    pub async fn current_control(&self, bot_id: &str) -> Result<Option<ControlRow>, RecordsError> {
        let row = sqlx::query(
            // RFC 3339, not Postgres's `2026-08-12 09:58:31.9+00`. The browser
            // parses that one only because V8 is lenient; Safari returns NaN,
            // and a NaN timestamp here reads as "no pending control", which is
            // the green RUNNING lie all over again in one browser and not the
            // other. Emit the unambiguous form and the question cannot arise.
            "SELECT state, reason, set_by, to_char(at AT TIME ZONE 'UTC', \
             'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'), payload::text \
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

    /// Narrate. Bounded by usage, not by schema: bots write what an
    /// operator would want at 3am, not a debug stream.
    pub async fn log_line(
        &self,
        bot_id: &str,
        level: &str,
        line: &str,
    ) -> Result<(), RecordsError> {
        sqlx::query("INSERT INTO bot_log (bot_id, level, line) VALUES ($1, $2, $3)")
            .bind(bot_id)
            .bind(level)
            .bind(line)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Newest first, as (at, level, line).
    pub async fn recent_log(
        &self,
        bot_id: &str,
        limit: i64,
    ) -> Result<Vec<(String, String, String)>, RecordsError> {
        let rows = sqlx::query(
            "SELECT at::text, level, line FROM bot_log WHERE bot_id = $1 \
             ORDER BY seq DESC LIMIT $2",
        )
        .bind(bot_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get(0), r.get(1), r.get(2)))
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

/// Wake the caller the moment a control is written for `bot_id`.
///
/// The operator's kill switch used to travel by poll: press Stop, wait for
/// the bot's next control read. Postgres pushes instead — a trigger on
/// control_events NOTIFYs, this LISTENs, and the returned channel yields
/// within milliseconds of the INSERT committing.
///
/// The channel closing means the listener connection died. Callers must
/// treat that as "go back to polling", never as "no more controls" — a
/// dropped LISTEN must degrade to slow, not to deaf.
pub async fn listen_controls(
    database_url: &str,
    bot_id: &str,
) -> Result<tokio::sync::mpsc::Receiver<()>, RecordsError> {
    let mut listener = sqlx::postgres::PgListener::connect(database_url).await?;
    listener.listen("bot_control").await?;
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let bot_id = bot_id.to_string();
    tokio::spawn(async move {
        loop {
            match listener.recv().await {
                Ok(n) if n.payload() == bot_id => {
                    // A full channel just means a wake-up is already
                    // queued; coalescing is the point.
                    let _ = tx.try_send(());
                }
                Ok(_) => {}
                Err(_) => return, // rx closes; the caller's poll takes over
            }
        }
    });
    Ok(rx)
}

/// Synchronous facade for callers that are not async (the runner's money
/// path). One current-thread runtime, one pool, shared behind an `Arc`.
pub mod blocking {
    use std::sync::Arc;

    use super::{ControlRow, RecordsError, Registry, StatusRow};

    pub struct Records {
        rt: Option<tokio::runtime::Runtime>,
        inner: Registry,
    }

    impl Drop for Records {
        fn drop(&mut self) {
            // A Runtime dropped inside an async context panics tokio;
            // shutdown_background never blocks, so this handle is safe to
            // drop anywhere — including an async caller's error path.
            if let Some(rt) = self.rt.take() {
                rt.shutdown_background();
            }
        }
    }

    impl Records {
        pub fn connect(database_url: &str) -> Result<Arc<Self>, RecordsError> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio current-thread runtime");
            let inner = wait_on(&rt, Registry::connect(database_url))?;
            Ok(Arc::new(Self {
                rt: Some(rt),
                inner,
            }))
        }

        pub fn registry(&self) -> &Registry {
            &self.inner
        }

        fn wait<F>(&self, fut: F) -> F::Output
        where
            F: std::future::Future + Send,
            F::Output: Send,
        {
            wait_on(self.rt.as_ref().expect("runtime lives until drop"), fut)
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

        pub fn record_plan(
            &self,
            bot_id: &str,
            plan_id: &str,
            run_id: Option<&str>,
            as_of: &str,
            created_at: &str,
            payload_json: &str,
        ) -> Result<(), RecordsError> {
            self.wait(self.inner.record_plan(
                bot_id,
                plan_id,
                run_id,
                as_of,
                created_at,
                payload_json,
            ))
        }

        pub fn plan_for_run(
            &self,
            bot_id: &str,
            run_id: &str,
        ) -> Result<Option<String>, RecordsError> {
            self.wait(self.inner.plan_for_run(bot_id, run_id))
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
            self.wait(
                self.inner
                    .set_control(bot_id, state, reason, set_by, payload_json),
            )
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
                bot_id,
                fill_key,
                at,
                instrument,
                sleeve,
                side,
                qty,
                price,
                pnl,
                reason,
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

        pub fn log_line(&self, bot_id: &str, level: &str, line: &str) -> Result<(), RecordsError> {
            self.wait(self.inner.log_line(bot_id, level, line))
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
