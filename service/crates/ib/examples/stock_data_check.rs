//! Read-only smoke test for the reusable IB stock data source.
//!
//! Usage (with IB_GATEWAY_* and IB_PAPER_ACCOUNT already exported):
//! `cargo run -p ib --example stock_data_check -- 917920 10`

use ib::stocks::{DailySeries, StockDataSource, StockQuery};
use ib::GatewayConfig;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let conid: i32 = args
        .next()
        .ok_or("usage: stock_data_check CONID [YEARS]")?
        .parse()?;
    let years: i32 = args.next().unwrap_or_else(|| "10".into()).parse()?;
    let client_id = std::env::var("IB_STOCK_DATA_CLIENT_ID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(18);

    let source = StockDataSource::connect(GatewayConfig::from_env(false, client_id)?).await?;
    let stock = source.resolve(&StockQuery::by_conid(conid)).await?;
    let trades = source
        .daily_history(&stock, DailySeries::Trades, years)
        .await?;
    let adjusted = source
        .daily_history(&stock, DailySeries::AdjustedLast, years)
        .await?;
    let fee_rate = source
        .daily_history(&stock, DailySeries::FeeRate, years)
        .await?;
    let snapshot = source.snapshot(&stock).await?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "account": source.account(),
            "paper": source.is_paper(),
            "contract": stock,
            "trades": coverage(&trades),
            "adjusted_last": coverage(&adjusted),
            "fee_rate": coverage(&fee_rate),
            "snapshot": snapshot,
        }))?
    );
    Ok(())
}

fn coverage(bars: &[ib::stocks::DailyBar]) -> serde_json::Value {
    json!({
        "count": bars.len(),
        "first": bars.first().map(|bar| bar.date.to_string()),
        "last": bars.last().map(|bar| bar.date.to_string()),
    })
}
