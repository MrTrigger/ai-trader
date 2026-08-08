"""Pull Binance USD-M perpetual funding history, and record WHICH assets have a perp.

Two things the long/short probe silently assumed and should not have:

**That anything can be shorted.** Only perp-listed assets can, and perps listed
at different dates. An asset with no perp at time T is not shortable at time T,
full stop — so the short leg has to be restricted to what actually existed.

**That shorting is free.** It is not: a perp position pays or receives funding
every 8 hours. For a dollar-neutral book the naive expectation is that it nets
out, but the long leg is uptrending and the short leg is downtrending, and
uptrending assets attract leveraged longs and therefore *positive* funding. A
book long high-funding and short low-funding pays the difference, every 8 hours,
forever. That is exactly the kind of cost that quietly eats a 15% CAGR.
"""

from __future__ import annotations

import json
import sys
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path

import polars as pl

from planner import store

OUT = Path(sys.argv[1])
START_MS = int(datetime(2021, 1, 1, tzinfo=timezone.utc).timestamp() * 1000)
WORKERS = 8


def get(url: str, tries: int = 4):
    last = None
    for attempt in range(tries):
        try:
            with urllib.request.urlopen(url, timeout=30) as r:
                return json.loads(r.read())
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(1.5 * (attempt + 1))
    raise last


info = get("https://fapi.binance.com/fapi/v1/exchangeInfo")
perps = {
    s["baseAsset"]: s
    for s in info["symbols"]
    if s["quoteAsset"] == "USDT" and s["contractType"] == "PERPETUAL"
}
print(f"USD-M perpetual USDT symbols: {len(perps)}", flush=True)

spot_assets = set(
    store.read(root=Path("/home/magnus/dev/magnus/ai-trader/data"), interval_s=86400)[
        "asset"
    ]
    .unique()
    .to_list()
)
targets = sorted(spot_assets & set(perps))
print(f"of our {len(spot_assets)} spot assets, {len(targets)} have a perp", flush=True)
print(f"  -> {len(spot_assets) - len(targets)} are NOT shortable at all", flush=True)


def funding(asset: str):
    symbol = perps[asset]["symbol"]
    rows, cursor = [], START_MS
    while True:
        batch = get(
            f"https://fapi.binance.com/fapi/v1/fundingRate?symbol={symbol}"
            f"&startTime={cursor}&limit=1000"
        )
        if not batch:
            break
        rows.extend(batch)
        last = int(batch[-1]["fundingTime"])
        if len(batch) < 1000 or last <= cursor:
            break
        cursor = last + 1
        time.sleep(0.08)
    return asset, rows


records, done, t0 = [], 0, time.time()
with ThreadPoolExecutor(max_workers=WORKERS) as pool:
    futures = [pool.submit(funding, a) for a in targets]
    for fut in as_completed(futures):
        try:
            asset, rows = fut.result()
        except Exception as e:  # noqa: BLE001
            print(f"  failed: {e}", flush=True)
            continue
        for r in rows:
            records.append(
                {
                    "asset": asset,
                    "ts_utc": datetime.fromtimestamp(
                        int(r["fundingTime"]) / 1000, tz=timezone.utc
                    ),
                    "rate": float(r["fundingRate"]),
                }
            )
        done += 1
        if done % 40 == 0:
            print(f"  {done}/{len(targets)}  {time.time() - t0:.0f}s", flush=True)

df = pl.DataFrame(records)
# 8-hourly -> daily: funding is charged three times a day, so the daily cost of
# holding is the sum of the intervals that fell in the day.
daily = (
    df.with_columns(pl.col("ts_utc").dt.truncate("1d").alias("day"))
    .group_by(["asset", "day"])
    .agg(pl.col("rate").sum().alias("daily_rate"), pl.len().alias("intervals"))
    .sort(["asset", "day"])
)
daily.write_parquet(OUT)

print(f"\n{df.height} funding intervals, {daily.height} asset-days", flush=True)
print(f"assets with funding history: {daily['asset'].n_unique()}", flush=True)
first = daily.group_by("asset").agg(pl.col("day").min().alias("perp_from"))
print("\nwhen perps listed (a short is impossible before this):", flush=True)
print(first.sort("perp_from").head(4), flush=True)
print(f"\nmedian daily funding across everything: {daily['daily_rate'].median():.6f} "
      f"({daily['daily_rate'].median() * 365 * 100:.1f}% annualised)", flush=True)
print(f"WROTE {OUT}", flush=True)
