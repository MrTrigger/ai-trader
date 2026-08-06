//! Identity registries — the DB-first source of truth for WHO is trading
//! (architecture-review-multibot.md step 1).
//!
//! Scope is deliberately small: registries and lookups. Operational records
//! (plans, fills, runs, NAV) migrate here in step 4. Nothing in this crate
//! touches credentials — accounts carry a `credential_ref` naming the
//! SOPS/env entry, and resolution stays where it is today.
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
            "SELECT bot_id, display_name, cadence, asset_class, decision_core, enabled \
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
        }))
    }

    pub async fn list_bots(&self) -> Result<Vec<BotRow>, RecordsError> {
        let rows = sqlx::query(
            "SELECT bot_id, display_name, cadence, asset_class, decision_core, enabled \
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
