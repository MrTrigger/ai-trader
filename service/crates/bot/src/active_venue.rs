//! Choosing the venue at runtime, and the gates in front of the live one.
//!
//! # Three modes, not two
//!
//! - **paper** — the fake broker over whatever price feed is configured. With a
//!   live feed this is a real paper run: prices move, fills slip, P&L happens.
//!   With a file feed it is a fixture, useful for tests and nothing else.
//! - **live-readonly** — the real venue, real balances and positions and fills,
//!   and every order refused. This is what to run while watching a real account
//!   before trusting anything to trade it.
//! - **live** — the real venue, able to trade.
//!
//! The middle one is the point. Going from "reading a real account" to "trading
//! it" should be one deliberate step, not the same step as "connecting at all".
//!
//! # What live costs you
//!
//! Three separate things must line up: `mode = "live"` in the config, an agent
//! key in the environment, and `HL_ALLOW_LIVE=yes`. They live in different
//! places on purpose — a config copied between machines carries the mode, a
//! `.env` copied carries the key, and needing both plus an explicit switch means
//! no single careless copy starts trading real money.

use std::sync::Arc;

use async_trait::async_trait;
use hyperliquid::Hyperliquid;
use paper::{PaperConfig, PaperVenue};
use time::OffsetDateTime;
use venue::{
    Balance, Fill, Market, OpenOrder, OrderAck, OrderRequest, Position, PriceSource, SystemClock,
    VenueAdapter, VenueError,
};

/// Which venue a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Paper,
    LiveReadonly,
    Live,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Paper => "paper",
            Mode::LiveReadonly => "live-readonly",
            Mode::Live => "live",
        }
    }

    /// Whether orders can actually reach a real venue in this mode.
    pub fn moves_real_money(self) -> bool {
        matches!(self, Mode::Live)
    }
}

impl std::str::FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "paper" => Ok(Mode::Paper),
            "live-readonly" | "live_readonly" => Ok(Mode::LiveReadonly),
            "live" => Ok(Mode::Live),
            other => Err(format!(
                "unknown mode '{other}'. One of: paper, live-readonly, live."
            )),
        }
    }
}

/// The venue a run is actually using.
///
/// An enum rather than a boxed trait object so the compiler still sees both
/// arms: adding a method to `VenueAdapter` fails to build here until both are
/// handled, which is what stops the live path quietly missing something the
/// paper path has.
pub enum Active {
    Paper(Box<PaperVenue<Arc<dyn PriceSource>, SystemClock>>),
    Live(Box<Hyperliquid>),
}

impl std::fmt::Debug for Active {
    /// Hand-written, like the live client's: this is the type a test or a log
    /// line reaches for, and a derived impl would print whatever the inner
    /// venue holds — including, one refactor from now, a key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Active({})", self.describe())
    }
}

impl Active {
    pub fn describe(&self) -> String {
        match self {
            Active::Paper(_) => "paper venue".into(),
            Active::Live(h) => format!(
                "hyperliquid {} for {}{}",
                if h.is_mainnet() { "mainnet" } else { "testnet" },
                h.account(),
                if h.can_trade() { "" } else { " (read-only)" }
            ),
        }
    }

    /// The address the venue will attribute our orders to, when there is a key.
    ///
    /// Worth surfacing: an agent that was never approved on the account, or has
    /// since expired, is the most common reason a first live order is rejected,
    /// and the rejection does not say which agent it disbelieved.
    pub fn agent_address(&self) -> Option<&str> {
        match self {
            Active::Live(h) => h.agent_address(),
            _ => None,
        }
    }

    /// Paper keeps its books in a file; the live venue keeps them itself.
    pub fn is_paper(&self) -> bool {
        matches!(self, Active::Paper(_))
    }

    pub fn as_paper(&self) -> Option<&PaperVenue<Arc<dyn PriceSource>, SystemClock>> {
        match self {
            Active::Paper(p) => Some(p),
            _ => None,
        }
    }
}

#[async_trait]
impl VenueAdapter for Active {
    async fn get_markets(&self) -> Result<Vec<Market>, VenueError> {
        match self {
            Active::Paper(v) => v.get_markets().await,
            Active::Live(v) => v.get_markets().await,
        }
    }
    async fn get_balances(&self) -> Result<Vec<Balance>, VenueError> {
        match self {
            Active::Paper(v) => v.get_balances().await,
            Active::Live(v) => v.get_balances().await,
        }
    }
    async fn get_positions(&self) -> Result<Vec<Position>, VenueError> {
        match self {
            Active::Paper(v) => v.get_positions().await,
            Active::Live(v) => v.get_positions().await,
        }
    }
    async fn get_open_orders(&self) -> Result<Vec<OpenOrder>, VenueError> {
        match self {
            Active::Paper(v) => v.get_open_orders().await,
            Active::Live(v) => v.get_open_orders().await,
        }
    }
    async fn get_fills(&self, since: Option<OffsetDateTime>) -> Result<Vec<Fill>, VenueError> {
        match self {
            Active::Paper(v) => v.get_fills(since).await,
            Active::Live(v) => v.get_fills(since).await,
        }
    }
    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError> {
        match self {
            Active::Paper(v) => v.place_order(order).await,
            Active::Live(v) => v.place_order(order).await,
        }
    }
    async fn cancel_order(&self, id: &str) -> Result<(), VenueError> {
        match self {
            Active::Paper(v) => v.cancel_order(id).await,
            Active::Live(v) => v.cancel_order(id).await,
        }
    }
}

/// Credentials and endpoints, read from the environment rather than the config.
///
/// Split on purpose: the config says *what to do*, the environment says *what
/// you are allowed to do it with*. A config file is committed, copied and
/// diffed; a key must not be any of those things.
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub api_url: Option<String>,
    pub account: Option<String>,
    pub vault: Option<String>,
    pub agent_key: Option<String>,
    pub allow_live: bool,
}

impl Env {
    /// Read from the process environment, having loaded `.env` if present.
    pub fn load(dotenv: Option<&std::path::Path>) -> Self {
        if let Some(p) = dotenv {
            load_dotenv(p);
        }
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Env {
            api_url: get("HL_API_URL"),
            account: get("HL_ACCOUNT_ADDRESS"),
            vault: get("HL_VAULT_ADDRESS"),
            agent_key: get("HL_AGENT_PRIVATE_KEY"),
            allow_live: get("HL_ALLOW_LIVE").as_deref() == Some("yes"),
        }
    }

    fn base(&self) -> &str {
        self.api_url.as_deref().unwrap_or(hyperliquid::MAINNET)
    }

    /// A live price feed, if the environment describes one.
    pub fn price_source(&self) -> Result<Arc<dyn PriceSource>, String> {
        let account = self.account.clone().unwrap_or_default();
        let hl = Hyperliquid::read_only(self.base(), &account)
            .map_err(|e| format!("cannot reach {}: {e}", self.base()))?;
        Ok(Arc::new(hl))
    }
}

/// Read `KEY=value` lines into the process environment without overwriting
/// anything already set.
///
/// A real shell export must win over a file: that is how you override one value
/// for one command without editing anything.
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
        let k = k.trim();
        let v = v.trim().trim_matches('"').trim_matches('\'');
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
}

/// Build the venue for a mode, refusing rather than degrading.
///
/// Every refusal names the specific thing that is missing. "Cannot start in
/// live mode" is useless at 3am; "HL_AGENT_PRIVATE_KEY is empty" is a fix.
pub fn open(
    mode: Mode,
    env: &Env,
    paper_cfg: PaperConfig,
    markets: Vec<Market>,
    prices: Arc<dyn PriceSource>,
    snapshot: Option<&str>,
) -> Result<Active, String> {
    match mode {
        Mode::Paper => {
            let v = match snapshot {
                Some(s) => PaperVenue::restore(paper_cfg, markets, prices, SystemClock, s)
                    .map_err(|e| {
                        format!(
                            "venue state is unreadable ({e}). Refusing to start with an empty \
                             book: that would look like a flat account and trade as one."
                        )
                    })?,
                None => PaperVenue::new(paper_cfg, markets, prices, SystemClock),
            };
            Ok(Active::Paper(Box::new(v)))
        }
        Mode::LiveReadonly | Mode::Live => {
            let account = env
                .account
                .as_deref()
                .filter(|a| {
                    // The placeholder in .env.example is a real address and would
                    // read as a permanently empty account rather than as an error.
                    *a != "0x0000000000000000000000000000000000000000"
                })
                .ok_or_else(|| {
                    "HL_ACCOUNT_ADDRESS is not set (or is still the placeholder). Live modes need \
                 to know whose book to read."
                        .to_string()
                })?;

            let hl = if mode == Mode::Live {
                let key = env.agent_key.as_deref().ok_or_else(|| {
                    "HL_AGENT_PRIVATE_KEY is empty, so this cannot trade. Generate an API \
                     wallet in the Hyperliquid UI, or run mode 'live-readonly' to watch the \
                     account without trading it."
                        .to_string()
                })?;
                Hyperliquid::trading(env.base(), account, key, env.vault.clone(), env.allow_live)
                    .map_err(|e| e.to_string())?
            } else {
                Hyperliquid::read_only(env.base(), account).map_err(|e| e.to_string())?
            };
            Ok(Active::Live(Box::new(hl)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper_cfg() -> PaperConfig {
        PaperConfig::default()
    }

    #[test]
    fn modes_round_trip_through_their_names() {
        for m in [Mode::Paper, Mode::LiveReadonly, Mode::Live] {
            assert_eq!(m.as_str().parse::<Mode>().unwrap(), m);
        }
        assert!("nonsense".parse::<Mode>().is_err());
        // Only one mode can lose money, and the other two must never claim to.
        assert!(Mode::Live.moves_real_money());
        assert!(!Mode::LiveReadonly.moves_real_money());
        assert!(!Mode::Paper.moves_real_money());
    }

    #[test]
    fn live_without_an_account_refuses_and_names_the_variable() {
        let env = Env::default();
        let e = open(Mode::Live, &env, paper_cfg(), vec![], dummy_prices(), None).unwrap_err();
        assert!(e.contains("HL_ACCOUNT_ADDRESS"), "{e}");
    }

    #[test]
    fn the_placeholder_address_is_treated_as_unset() {
        // Copying .env.example without editing it must not silently produce a
        // client pointed at the zero address, which reads as a flat account
        // forever rather than as a mistake.
        let env = Env {
            account: Some("0x0000000000000000000000000000000000000000".into()),
            ..Default::default()
        };
        let e = open(
            Mode::LiveReadonly,
            &env,
            paper_cfg(),
            vec![],
            dummy_prices(),
            None,
        )
        .unwrap_err();
        assert!(e.contains("placeholder"), "{e}");
    }

    #[test]
    fn live_without_a_key_refuses_but_read_only_does_not() {
        let env = Env {
            account: Some("0x1111111111111111111111111111111111111111".into()),
            ..Default::default()
        };
        let e = open(Mode::Live, &env, paper_cfg(), vec![], dummy_prices(), None).unwrap_err();
        assert!(e.contains("HL_AGENT_PRIVATE_KEY"), "{e}");
        assert!(
            e.contains("live-readonly"),
            "and offers the safe alternative: {e}"
        );

        // Watching a real account needs no credential at all.
        let v = open(
            Mode::LiveReadonly,
            &env,
            paper_cfg(),
            vec![],
            dummy_prices(),
            None,
        )
        .unwrap();
        assert!(!v.is_paper());
    }

    #[test]
    fn mainnet_live_needs_the_environment_opt_in_as_well_as_the_key() {
        const KEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        let mut env = Env {
            account: Some("0x1111111111111111111111111111111111111111".into()),
            agent_key: Some(KEY.into()),
            api_url: Some(hyperliquid::MAINNET.into()),
            allow_live: false,
            ..Default::default()
        };
        let e = open(Mode::Live, &env, paper_cfg(), vec![], dummy_prices(), None).unwrap_err();
        assert!(e.contains("HL_ALLOW_LIVE"), "{e}");

        env.allow_live = true;
        assert!(open(Mode::Live, &env, paper_cfg(), vec![], dummy_prices(), None).is_ok());
    }

    #[test]
    fn a_dotenv_never_overrides_something_already_exported() {
        // Overriding one value for one command is the reason to export it, and
        // a file that wins would make that impossible.
        let dir = std::env::temp_dir().join(format!("dotenv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(".env");
        std::fs::write(
            &f,
            "AI_TRADER_TEST_A=from_file\nAI_TRADER_TEST_B=from_file\n",
        )
        .unwrap();
        std::env::set_var("AI_TRADER_TEST_A", "from_shell");
        load_dotenv(&f);
        assert_eq!(std::env::var("AI_TRADER_TEST_A").unwrap(), "from_shell");
        assert_eq!(std::env::var("AI_TRADER_TEST_B").unwrap(), "from_file");
    }

    fn dummy_prices() -> Arc<dyn PriceSource> {
        Arc::new(venue::ManualPrices::new())
    }
}
