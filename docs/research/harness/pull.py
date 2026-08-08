"""Pull every USDT spot asset Binance has ever listed, alive or dead.

Both TRADING and BREAK statuses, minus leveraged tokens. The dead are the whole
point: a store of survivors would make momentum look good precisely because it
is the wrong strategy.
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
from planner.sources import BinancePublic, is_leveraged_token

ROOT = Path(sys.argv[1])
START = datetime(2021, 1, 1, tzinfo=timezone.utc)
END = datetime(2026, 8, 4, tzinfo=timezone.utc)
WORKERS = 8

info = json.loads(
    urllib.request.urlopen(
        "https://api.binance.com/api/v3/exchangeInfo?permissions=SPOT", timeout=60
    ).read()
)
assets = sorted(
    {
        s["baseAsset"]
        for s in info["symbols"]
        if s["quoteAsset"] == "USDT" and not is_leveraged_token(s["baseAsset"])
    }
)
status = {s["baseAsset"]: s["status"] for s in info["symbols"] if s["quoteAsset"] == "USDT"}
dead = [a for a in assets if status.get(a) == "BREAK"]
print(f"assets: {len(assets)}  of which already delisted (BREAK): {len(dead)}", flush=True)


def fetch(asset: str):
    source = BinancePublic()
    try:
        df = source.fetch_bars(asset, interval_s=86_400, start=START, end=END)
        return asset, df, None
    except Exception as exc:  # noqa: BLE001 - a dead symbol is not a crash
        return asset, None, exc
    finally:
        source.close()


frames = []
failed = []
done = 0
t0 = time.time()

with ThreadPoolExecutor(max_workers=WORKERS) as pool:
    futures = [pool.submit(fetch, a) for a in assets]
    for future in as_completed(futures):
        asset, df, err = future.result()
        done += 1
        if err is not None:
            failed.append((asset, str(err)[:80]))
        elif df is not None and not df.is_empty():
            frames.append(df)
        if done % 50 == 0:
            print(f"  {done}/{len(assets)}  {time.time() - t0:.0f}s", flush=True)

combined = pl.concat(frames)
print(f"fetched {len(frames)} assets, {combined.height} bars, {len(failed)} failures", flush=True)
issues = store.write(combined, root=ROOT, source="binance_public")
print("store issues:", issues, flush=True)

alive = combined.group_by("asset").agg(pl.col("ts_utc").max().alias("last"))
stopped = alive.filter(pl.col("last") < datetime(2026, 7, 1, tzinfo=timezone.utc))
print(f"assets whose series ends before 2026-07: {stopped.height} (these are the dead)", flush=True)
print("DONE", flush=True)
