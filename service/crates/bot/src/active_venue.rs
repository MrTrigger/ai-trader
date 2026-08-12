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
/// Paper stays a concrete type (its books are OURS to snapshot); the live
/// side is a type-erased adapter plus the metadata captured when the venue
/// registry opened it. Identity step 2: nothing outside [`open_live`] may
/// know a live venue's name.
pub enum Active {
    Paper(Box<PaperVenue<PaperUpstream, SystemClock>>),
    Live {
        adapter: Box<dyn VenueAdapter + Send + Sync>,
        /// Human description captured at open ("hyperliquid mainnet for 0x…").
        description: String,
        /// The signing agent, when the venue has that concept.
        agent: Option<String>,
    },
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
            Active::Live { description, .. } => description.clone(),
        }
    }

    /// The address the venue will attribute our orders to, when there is a key.
    ///
    /// Worth surfacing: an agent that was never approved on the account, or has
    /// since expired, is the most common reason a first live order is rejected,
    /// and the rejection does not say which agent it disbelieved.
    pub fn agent_address(&self) -> Option<&str> {
        match self {
            Active::Live { agent, .. } => agent.as_deref(),
            _ => None,
        }
    }

    /// Paper keeps its books in a file; the live venue keeps them itself.
    pub fn is_paper(&self) -> bool {
        matches!(self, Active::Paper(_))
    }

    pub fn as_paper(&self) -> Option<&PaperVenue<PaperUpstream, SystemClock>> {
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
            Active::Live { adapter, .. } => adapter.get_markets().await,
        }
    }
    async fn get_balances(&self) -> Result<Vec<Balance>, VenueError> {
        match self {
            Active::Paper(v) => v.get_balances().await,
            Active::Live { adapter, .. } => adapter.get_balances().await,
        }
    }
    async fn get_positions(&self) -> Result<Vec<Position>, VenueError> {
        match self {
            Active::Paper(v) => v.get_positions().await,
            Active::Live { adapter, .. } => adapter.get_positions().await,
        }
    }
    async fn get_open_orders(&self) -> Result<Vec<OpenOrder>, VenueError> {
        match self {
            Active::Paper(v) => v.get_open_orders().await,
            Active::Live { adapter, .. } => adapter.get_open_orders().await,
        }
    }
    async fn get_fills(&self, since: Option<OffsetDateTime>) -> Result<Vec<Fill>, VenueError> {
        match self {
            Active::Paper(v) => v.get_fills(since).await,
            Active::Live { adapter, .. } => adapter.get_fills(since).await,
        }
    }
    /// Delegated like everything else here. `get_order_book` has a default so
    /// a venue without depth blocks nobody, and the price of that default is
    /// that every hand-written impl between the caller and the exchange must
    /// remember to pass it on. There are three of them - this, `PaperUpstream`,
    /// and the paper venue - and any one forgetting turns a venue that
    /// publishes full depth into one that "does not support order book".
    async fn get_order_book(&self, asset: &str) -> Result<venue::OrderBook, VenueError> {
        match self {
            Active::Paper(v) => v.get_order_book(asset).await,
            Active::Live { adapter, .. } => adapter.get_order_book(asset).await,
        }
    }
    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError> {
        match self {
            Active::Paper(v) => v.place_order(order).await,
            Active::Live { adapter, .. } => adapter.place_order(order).await,
        }
    }
    async fn cancel_order(&self, id: &str) -> Result<(), VenueError> {
        match self {
            Active::Paper(v) => v.cancel_order(id).await,
            Active::Live { adapter, .. } => adapter.cancel_order(id).await,
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
    /// What the agent is called in the Hyperliquid UI. Cosmetic, and worth
    /// having: agents expire, and "which of these three is the bot using" is
    /// otherwise unanswerable.
    pub agent_name: Option<String>,
    /// The agent address the UI showed. Optional, and checked against the one
    /// the key derives — see [`Env::agent_mismatch`].
    pub agent_address: Option<String>,
    pub allow_live: bool,
}

impl Env {
    /// Load `.env` into the process environment, then read it.
    ///
    /// Called once at startup so that every gate — the identity registry
    /// included — sees the same environment. Loading it lazily, when the venue
    /// was opened, meant checks that run earlier silently saw an empty one.
    pub fn load(dotenv: Option<&std::path::Path>) -> Self {
        if let Some(p) = dotenv {
            load_dotenv(p);
        }
        Self::from_process()
    }

    /// Read what is already in the environment, loading nothing.
    pub fn from_process() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Env {
            api_url: get("HL_API_URL"),
            account: get("HL_ACCOUNT_ADDRESS"),
            vault: get("HL_VAULT_ADDRESS"),
            agent_key: get("HL_AGENT_PRIVATE_KEY"),
            agent_name: get("HL_AGENT_NAME"),
            agent_address: get("HL_AGENT_ADDRESS"),
            allow_live: get("HL_ALLOW_LIVE").as_deref() == Some("yes"),
        }
    }

    /// The address the configured key actually signs as.
    pub fn derived_agent(&self) -> Option<String> {
        self.agent_key
            .as_deref()
            .and_then(|k| hyperliquid::Agent::from_hex(k).ok())
            .map(|a| a.address().to_string())
    }

    /// A complaint if the declared agent address and the key disagree.
    ///
    /// This is the check the address is here for. A key with a character
    /// dropped still parses, still signs, and produces signatures for a
    /// different wallet entirely — one the account never approved. The venue
    /// rejects those without saying whose signature it disbelieved, so the
    /// mismatch has to be caught here or not at all.
    pub fn agent_mismatch(&self) -> Option<String> {
        let declared = self.agent_address.as_deref()?.trim().to_lowercase();
        let derived = self.derived_agent()?.to_lowercase();
        if declared == derived {
            return None;
        }
        Some(format!(
            "HL_AGENT_ADDRESS says {declared} but HL_AGENT_PRIVATE_KEY signs as {derived}. One \
             of the two was pasted wrong. A key with a character missing still signs - it just \
             signs as a wallet your account never approved, and the venue rejects that without \
             saying whose signature it disbelieved."
        ))
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
/// The instrument universe a paper book should trade, taken from the venue.
///
/// Paper used to get its markets from a hand-written list in the bot config,
/// which made the simulator a different venue from the one it stands in for:
/// it refused `AAVE` because the list was short, while Hyperliquid lists it,
/// and it would have happily filled a lot size the real venue rejects. Every
/// exchange needed its own hand-maintained copy, so paper behaved differently
/// per venue for no reason anyone chose.
///
/// Reading markets is public data - no signing key - so this works for any
/// venue the bot can name, and the paper book inherits that venue's real
/// tick, lot, min-notional and listing set.
///
/// Returns `Ok(None)` rather than an error when the venue cannot be reached:
/// paper must still run offline against a fixture feed, and that is exactly
/// when the configured list is the right fallback.
pub async fn venue_markets(venue_id: &str, env: &Env) -> Result<Option<Vec<Market>>, String> {
    let adapter = match open_live(venue_id, Mode::LiveReadonly, env) {
        Ok(Active::Live { adapter, .. }) => adapter,
        Ok(_) => return Ok(None),
        // Not configured for this venue (no address, offline fixture run, an
        // unknown venue id): the caller falls back to the configured list.
        Err(_) => return Ok(None),
    };
    match adapter.get_markets().await {
        Ok(m) if !m.is_empty() => Ok(Some(m)),
        Ok(_) => Ok(None),
        Err(e) => Err(format!(
            "cannot read the market list from {venue_id}: {e}. Paper takes its instrument \
             universe from the venue so that a simulated fill is one the venue would have \
             accepted; fix the connection, or list `markets` in the bot config to run offline."
        )),
    }
}

/// Every refusal names the specific thing that is missing. "Cannot start in
/// live mode" is useless at 3am; "HL_AGENT_PRIVATE_KEY is empty" is a fix.
pub fn open(
    venue_id: &str,
    mode: Mode,
    env: &Env,
    paper_cfg: PaperConfig,
    prices: Option<Arc<dyn PriceSource>>,
    snapshot: Option<&str>,
) -> Result<Active, String> {
    match mode {
        Mode::Paper => {
            // Paper wraps the live adapter rather than standing beside it. The
            // exchange is the data provider in paper exactly as in live, so the
            // instrument universe, the tick and lot grids and the marks are all
            // read from it; only order submission is simulated. There is no
            // second list anywhere, which is the point - a hand-kept copy is
            // how paper stops predicting live.
            let upstream = match open_live(venue_id, Mode::LiveReadonly, env)? {
                Active::Live { adapter, .. } => adapter,
                other => return Ok(other),
            };
            let upstream: Arc<dyn VenueAdapter + Send + Sync> = Arc::from(upstream);
            // Marks: the exchange's own feed unless a file feed was configured
            // for offline work. Either way the instruments come from `upstream`.
            let prices = match prices {
                Some(p) => p,
                None => env.price_source()?,
            };
            let source = PaperUpstream {
                venue: upstream,
                prices,
            };
            let v = match snapshot {
                Some(s) => PaperVenue::restore(paper_cfg, source, SystemClock, s).map_err(|e| {
                    format!(
                        "venue state is unreadable ({e}). Refusing to start with an empty \
                         book: that would look like a flat account and trade as one."
                    )
                })?,
                None => PaperVenue::new(paper_cfg, source, SystemClock),
            };
            Ok(Active::Paper(Box::new(v)))
        }
        Mode::LiveReadonly | Mode::Live => open_live(venue_id, mode, env),
    }
}

/// The exchange, with the marks optionally overridden by a file feed.
pub struct PaperUpstream {
    /// The exchange, for everything it knows about its own instruments.
    venue: Arc<dyn VenueAdapter + Send + Sync>,
    /// The exchange's price feed, or a file feed for offline work. Marks are a
    /// separate handle only because `mark_price` lives on `PriceSource`; in
    /// normal operation both of these are the same Hyperliquid connection.
    prices: Arc<dyn PriceSource>,
}

#[async_trait::async_trait]
impl venue::MarketData for PaperUpstream {
    async fn markets(&self) -> Result<Vec<Market>, venue::VenueError> {
        self.venue.get_markets().await
    }
    async fn mark(&self, asset: &str) -> Result<rust_decimal::Decimal, venue::VenueError> {
        self.prices.mark_price(asset).await
    }
    /// Forwarded to the exchange like `markets`, and for the same reason: the
    /// paper book invents fills, never market data. Written out rather than
    /// inherited because this type implements `MarketData` by hand, so it
    /// silently took the trait's "unsupported" default and reported that the
    /// exchange has no order book.
    async fn order_book(&self, asset: &str) -> Result<venue::OrderBook, venue::VenueError> {
        self.venue.get_order_book(asset).await
    }
}

/// THE venue registry: the one sanctioned place a live venue's name may be
/// matched (spec section 4.1 — nothing else branches on venue identity).
/// Authority (`mode`) and identity (`venue_id`) arrive as separate inputs;
/// conflating them was the step-2 finding.
fn open_live(venue_id: &str, mode: Mode, env: &Env) -> Result<Active, String> {
    match venue_id {
        "hyperliquid" => open_hyperliquid(mode, env),
        "ib" => open_ib(mode),
        // Broker names, not protocol names: AMP is the FCM, Rithmic is the
        // gateway it is reached through. This process has no registry to
        // consult, so the mapping is spelled out here; the futures bot,
        // which does have one, dispatches on the venue's `protocol` column.
        "amp" | "rithmic" => open_rithmic(mode),
        other => Err(format!(
            "unknown venue_id {other:?}. Known live venues: hyperliquid, ib, amp."
        )),
    }
}

fn open_ib(mode: Mode) -> Result<Active, String> {
    // The account block follows the arming intent: the LIVE block only when
    // the mode is live AND the operator set IB_ALLOW_LIVE; anything else
    // uses the paper block. Read-only mode forces orders off regardless of
    // the flags — authority comes from `mode`, never from env alone.
    let allow_live = matches!(
        std::env::var("IB_ALLOW_LIVE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "yes" | "true" | "1"
    );
    let live_block = mode == Mode::Live && allow_live;
    let mut cfg = ib::IbConfig::from_env(live_block)?;
    if mode == Mode::LiveReadonly {
        cfg.allow_orders = false;
    }
    let adapter = ib::IbLazy::new(cfg);
    let description = adapter.describe();
    Ok(Active::Live {
        adapter: Box::new(adapter),
        description,
        agent: None,
    })
}

fn open_rithmic(mode: Mode) -> Result<Active, String> {
    let allow_live = matches!(
        std::env::var("RITHMIC_ALLOW_LIVE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "yes" | "true" | "1"
    );
    let live_env = mode == Mode::Live && allow_live;
    let mut cfg = rithmic::RithmicCfg::from_env(live_env)?;
    if mode == Mode::LiveReadonly {
        cfg.allow_orders = false;
    }
    let adapter = rithmic::RithmicLazy::new(cfg);
    let description = adapter.describe();
    Ok(Active::Live {
        adapter: Box::new(adapter),
        description,
        agent: None,
    })
}

fn open_hyperliquid(mode: Mode, env: &Env) -> Result<Active, String> {
    {
        {
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

            if let Some(problem) = env.agent_mismatch() {
                return Err(problem);
            }

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
            let description = format!(
                "hyperliquid {} for {}{}",
                if hl.is_mainnet() {
                    "mainnet"
                } else {
                    "testnet"
                },
                hl.account(),
                if hl.can_trade() { "" } else { " (read-only)" }
            );
            let agent = hl.agent_address().map(str::to_string);
            Ok(Active::Live {
                adapter: Box::new(hl),
                description,
                agent,
            })
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
        let e = open(
            "hyperliquid",
            Mode::Live,
            &env,
            paper_cfg(),
            Some(dummy_prices()),
            None,
        )
        .unwrap_err();
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
            "hyperliquid",
            Mode::LiveReadonly,
            &env,
            paper_cfg(),
            Some(dummy_prices()),
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
        let e = open(
            "hyperliquid",
            Mode::Live,
            &env,
            paper_cfg(),
            Some(dummy_prices()),
            None,
        )
        .unwrap_err();
        assert!(e.contains("HL_AGENT_PRIVATE_KEY"), "{e}");
        assert!(
            e.contains("live-readonly"),
            "and offers the safe alternative: {e}"
        );

        // Watching a real account needs no credential at all.
        let v = open(
            "hyperliquid",
            Mode::LiveReadonly,
            &env,
            paper_cfg(),
            Some(dummy_prices()),
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
        let e = open(
            "hyperliquid",
            Mode::Live,
            &env,
            paper_cfg(),
            Some(dummy_prices()),
            None,
        )
        .unwrap_err();
        assert!(e.contains("HL_ALLOW_LIVE"), "{e}");

        env.allow_live = true;
        assert!(open(
            "hyperliquid",
            Mode::Live,
            &env,
            paper_cfg(),
            Some(dummy_prices()),
            None
        )
        .is_ok());
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

    const KEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
    const KEY_ADDRESS: &str = "0x2c7536e3605d9c16a7a3d7b1898e529396a65c23";

    #[test]
    fn a_declared_agent_that_matches_the_key_is_silent() {
        let env = Env {
            agent_key: Some(KEY.into()),
            agent_address: Some(KEY_ADDRESS.to_uppercase()),
            ..Default::default()
        };
        // Case must not matter: the UI shows a checksummed address and people
        // paste it as they see it.
        assert_eq!(env.agent_mismatch(), None);
        assert_eq!(env.derived_agent().as_deref(), Some(KEY_ADDRESS));
    }

    #[test]
    fn a_key_that_signs_as_someone_else_is_caught_before_anything_is_sent() {
        // The failure this exists for: a key with a character dropped still
        // parses and still signs - as a wallet the account never approved.
        let env = Env {
            agent_key: Some(KEY.into()),
            agent_address: Some("0xdeadbeef00000000000000000000000000000000".into()),
            ..Default::default()
        };
        let m = env.agent_mismatch().expect("mismatch must be reported");
        assert!(
            m.contains(KEY_ADDRESS),
            "names what the key actually signs as: {m}"
        );
        assert!(m.contains("deadbeef"), "and what was declared: {m}");
    }

    #[test]
    fn a_mismatch_stops_a_live_venue_from_opening_at_all() {
        let env = Env {
            account: Some("0x1111111111111111111111111111111111111111".into()),
            agent_key: Some(KEY.into()),
            agent_address: Some("0xdeadbeef00000000000000000000000000000000".into()),
            api_url: Some(hyperliquid::TESTNET.into()),
            ..Default::default()
        };
        let e = open(
            "hyperliquid",
            Mode::Live,
            &env,
            paper_cfg(),
            Some(dummy_prices()),
            None,
        )
        .unwrap_err();
        assert!(e.contains("pasted wrong"), "{e}");
    }

    #[test]
    fn declaring_nothing_is_fine_because_the_address_is_optional() {
        let env = Env {
            agent_key: Some(KEY.into()),
            ..Default::default()
        };
        assert_eq!(env.agent_mismatch(), None);
    }

    fn dummy_prices() -> Arc<dyn PriceSource> {
        Arc::new(venue::ManualPrices::new())
    }
}
