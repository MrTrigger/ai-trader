use serde::{Deserialize, Serialize};

use crate::binance_um::binance_um_symbol;
use hyperliquid::AssetCtx;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub coin: String,
    pub day_volume_usd: f64,
    pub binance_um: Option<String>,
}

/// Select candidates from pairs sorted by volume.
///
/// - Filters to only pairs with positive `day_volume_usd()`
/// - Removes excluded coins
/// - Sorts by volume descending
/// - Takes the top N
/// - Maps each coin to its Binance UM symbol
pub fn select_candidates(
    pairs: &[(String, AssetCtx)],
    top: usize,
    exclude: &[String],
) -> Vec<Candidate> {
    let exclude_set: std::collections::HashSet<_> = exclude.iter().cloned().collect();

    let mut filtered: Vec<_> = pairs
        .iter()
        .filter(|(_, ctx)| ctx.day_volume_usd().unwrap_or(0.0) > 0.0)
        .filter(|(coin, _)| !exclude_set.contains(coin))
        .collect();

    filtered.sort_by(|a, b| {
        b.1.day_volume_usd()
            .unwrap_or(0.0)
            .total_cmp(&a.1.day_volume_usd().unwrap_or(0.0))
    });

    filtered
        .into_iter()
        .take(top)
        .map(|(coin, ctx)| Candidate {
            coin: coin.clone(),
            day_volume_usd: ctx.day_volume_usd().unwrap_or(0.0),
            binance_um: binance_um_symbol(coin),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_volume(v: &str) -> AssetCtx {
        serde_json::from_value(serde_json::json!({
            "dayNtlVlm": v
        }))
        .unwrap()
    }

    #[test]
    fn candidates_rank_by_volume_and_respect_exclusions() {
        let pairs = vec![
            ("BTC".to_string(), ctx_with_volume("300")),
            ("ETH".to_string(), ctx_with_volume("200")),
            ("kPEPE".to_string(), ctx_with_volume("100")),
        ];
        let out = select_candidates(&pairs, 2, &["ETH".to_string()]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].coin, "BTC");
        assert_eq!(out[1].coin, "kPEPE");
        assert_eq!(out[1].binance_um.as_deref(), Some("1000PEPEUSDT"));
    }
}
