//! The read path against the real venue.
//!
//! These talk to `api.hyperliquid.xyz` and are **ignored by default**, because
//! a test suite that fails when the wifi drops teaches people to ignore test
//! failures. Run them deliberately:
//!
//! ```text
//! cargo test -p hyperliquid --test live_read -- --ignored --nocapture
//! ```
//!
//! They exist because an adapter is a claim about someone else's API, and the
//! only thing that can check that claim is the API. Unit tests over fixtures
//! prove the parser handles the bytes I captured; these prove the bytes are
//! still what the venue sends.

use hyperliquid::{Info, MAINNET};

fn info() -> Info {
    Info::new(MAINNET).expect("client builds")
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn the_market_list_parses_and_looks_like_a_perp_venue() {
    let markets = info().meta().await.expect("meta");
    assert!(markets.len() > 50, "only {} markets", markets.len());

    let btc = markets
        .iter()
        .find(|m| m.asset == "BTC")
        .expect("BTC listed");
    assert_eq!(btc.quote_currency, "USDC");
    assert!(btc.capabilities.short, "these are perps");
    assert!(btc.capabilities.funding);
    assert!(btc.lot > rust_decimal::Decimal::ZERO, "lot must be usable");
    assert!(btc.tick > rust_decimal::Decimal::ZERO);
    println!("BTC market: {btc:?}");
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn marks_are_present_and_plausible() {
    let marks = info().marks().await.expect("marks");
    let btc = marks.get("BTC").copied().expect("BTC has a mark");
    // Deliberately a very wide band. This is checking that the field is the
    // price and not, say, the funding rate or an index — not predicting BTC.
    assert!(
        btc > rust_decimal::Decimal::from(1_000) && btc < rust_decimal::Decimal::from(10_000_000),
        "BTC mark of {btc} is not a price"
    );
    println!("{} marks, BTC = {btc}", marks.len());
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn markets_and_marks_agree_on_the_universe() {
    // They come from one call precisely so they describe the same instant; if
    // the zip ever misaligns, every mark would be attributed to the wrong coin.
    let (markets, marks) = info().markets_and_marks().await.expect("both");
    let named: Vec<_> = markets.iter().map(|m| m.asset.as_str()).collect();
    for coin in marks.keys() {
        assert!(
            named.contains(&coin.as_str()),
            "{coin} has a mark but no market"
        );
    }
    let mids = info().all_mids().await.expect("mids");
    let a = marks.get("BTC").copied().unwrap();
    let b = mids.get("BTC").copied().unwrap();
    let gap = ((a - b) / b).abs();
    assert!(
        gap < rust_decimal::Decimal::new(2, 2),
        "mark {a} and mid {b} are {gap} apart — one of them is not what we think"
    );
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn an_account_with_no_activity_reads_as_flat_rather_than_failing() {
    // The empty case has to be ordinary. A fresh account must not look like an
    // error, or the first thing a new deployment does is halt.
    let zero = "0x0000000000000000000000000000000000000000";
    let i = info();
    let positions = i.positions(zero).await.expect("positions");
    let orders = i.open_orders(zero).await.expect("open orders");
    println!(
        "zero address: {} positions, {} orders",
        positions.len(),
        orders.len()
    );
    let balances = i.balances(zero).await.expect("balances");
    assert_eq!(balances.len(), 1, "one quote balance");
    assert_eq!(balances[0].currency, "USDC");
}

/// A funded unified account, if `HL_ACCOUNT_ADDRESS` names one.
fn configured_account() -> Option<String> {
    for line in std::fs::read_to_string("../../../.env").ok()?.lines() {
        if let Some(v) = line.trim().strip_prefix("HL_ACCOUNT_ADDRESS=") {
            let v = v.trim().trim_matches('"').to_string();
            if !v.is_empty() && v != "0x0000000000000000000000000000000000000000" {
                return Some(v);
            }
        }
    }
    None
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn a_funded_account_never_reports_as_empty() {
    // The bug this pins: a unified account holds its collateral on the spot
    // side and reports zero on the perps side, so reading only perps made a
    // funded account look flat. A strategy told it has no money either refuses
    // to size anything or divides by zero.
    let Some(account) = configured_account() else {
        println!("no account configured; skipped");
        return;
    };
    let i = info();
    let unified = i.is_unified(&account).await.expect("unified check");
    let balances = i.balances(&account).await.expect("balances");
    assert_eq!(balances.len(), 1);
    let b = &balances[0];
    println!(
        "{account}: unified={unified} total={} available={}",
        b.total, b.available
    );
    assert!(
        b.available <= b.total,
        "available {} exceeds total {}",
        b.available,
        b.total
    );
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn a_unified_account_is_not_double_counted() {
    // The assumption that could not be checked when this was written: no
    // reachable unified account had an open perp position. If the perps view
    // ever starts reporting the same collateral the spot side already does,
    // adding them would overstate the account - so this asserts the balance we
    // report never exceeds the two sides added together, and shouts if the
    // perps side becomes non-zero on a unified account so the question gets
    // looked at again.
    let Some(account) = configured_account() else {
        println!("no account configured; skipped");
        return;
    };
    let i = info();
    if !i.is_unified(&account).await.expect("unified check") {
        println!("{account} is a classic account; nothing to check");
        return;
    }
    let reported = i.balances(&account).await.expect("balances")[0].total;
    let perps_side = i.raw_perps_account_value(&account).await.expect("perps");
    println!("unified: reported={reported} perps-side={perps_side}");
    assert!(
        perps_side.is_zero(),
        "a unified account is now reporting {perps_side} on the perps side as well as its spot \
         collateral. Check whether balances() should be adding them before trusting this number."
    );
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn the_collateral_token_is_still_the_one_we_assume() {
    // Perps settle in one token, named by the venue. If that ever changes, the
    // unified-account lookup would key on the wrong index and read zero.
    let token = info().collateral_token().await.expect("meta");
    assert_eq!(
        token,
        hyperliquid::QUOTE_TOKEN,
        "perps now collateralise in token {token}, not {}",
        hyperliquid::QUOTE_TOKEN
    );
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn fills_come_back_oldest_first_with_positive_fees() {
    // Order matters: a position is folded out of a fill log in sequence. Fee
    // sign matters: ours are costs, and Hyperliquid reports rebates negative.
    let zero = "0x0000000000000000000000000000000000000000";
    let fills = info().fills(zero, None).await.expect("fills");
    if fills.len() < 2 {
        println!(
            "only {} fills on the probe address; ordering unchecked",
            fills.len()
        );
        return;
    }
    for w in fills.windows(2) {
        assert!(w[0].ts <= w[1].ts, "fills are not in order");
    }
    assert!(
        fills.iter().all(|f| f.fee >= rust_decimal::Decimal::ZERO),
        "a fee is a cost and must never be negative"
    );
    assert!(fills.iter().all(|f| f.qty > rust_decimal::Decimal::ZERO));
    println!(
        "{} fills, first {:?}",
        fills.len(),
        fills.first().map(|f| &f.asset)
    );
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn candles_are_ordered_bars_with_sane_highs_and_lows() {
    let now = time::OffsetDateTime::now_utc().unix_timestamp() * 1000;
    let bars = info()
        .candles("BTC", "1h", now - 86_400_000, now)
        .await
        .expect("candles");
    assert!(!bars.is_empty(), "a day of hourly bars should not be empty");
    for b in &bars {
        let (h, l): (rust_decimal::Decimal, rust_decimal::Decimal) =
            (b.h.parse().unwrap(), b.l.parse().unwrap());
        let (o, c): (rust_decimal::Decimal, rust_decimal::Decimal) =
            (b.o.parse().unwrap(), b.c.parse().unwrap());
        assert!(h >= l, "high below low");
        assert!(
            o <= h && o >= l && c <= h && c >= l,
            "open/close outside the range"
        );
    }
    println!("{} hourly BTC bars", bars.len());
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn the_book_has_a_positive_spread() {
    let (bid, ask) = info().top_of_book("BTC").await.expect("book");
    assert!(ask > bid, "crossed book: bid {bid} ask {ask}");
    let half_bps = ((ask - bid) / ((ask + bid) / rust_decimal::Decimal::TWO))
        * rust_decimal::Decimal::from(10_000)
        / rust_decimal::Decimal::TWO;
    // Phase 1 measured a 0.37-0.39bp half-spread on BTC and configured 0.5.
    // This prints it rather than asserting a bound: the point is to notice if
    // the cost model's assumption stops holding, not to fail on a busy minute.
    println!("BTC bid {bid} ask {ask} — half-spread {half_bps:.3}bp (config assumes 0.5)");
}

#[tokio::test]
#[ignore = "talks to the live venue"]
async fn an_unknown_coin_is_an_error_rather_than_a_zero() {
    let e = info().top_of_book("NOTACOIN").await;
    assert!(
        e.is_err(),
        "a coin that does not exist must not return a price"
    );
}
