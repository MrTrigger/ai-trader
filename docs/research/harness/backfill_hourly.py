"""Backfill hourly bars for every asset the strategy actually trades.

The store has been daily-only because `interval_s = 86400` came from the Phase 0
config and was never revisited - not because anything measured said daily was
right. The layout already anticipated this: store.py documents
`interval_s=3600` partitioned monthly, and the archive source has supported 1h
all along.

It matters now because the daily backtest cannot answer an execution question.
Filling at the bar open means trading at the exact timestamp the features were
computed, and the daily book's Sharpe falls from 2.56 to 0.30 when that
assumption is relaxed to "sometime during the day". Hourly bars are what turn
that from a bracket into a measurement.

Resumable: any (asset, month) already in the store is skipped, so an interrupted
run costs only the batch in flight. Written in batches rather than per-month, to
avoid tens of thousands of tiny parquet files.
"""
import sys, time
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path

import polars as pl

from planner import store
from planner.sources.binance_archive import BinanceArchive

ROOT = Path("/home/magnus/dev/magnus/ai-trader")
DATA = ROOT / "data"
INTERVAL = 3600
START = datetime(2019, 10, 1, tzinfo=timezone.utc)
END = datetime(2026, 8, 1, tzinfo=timezone.utc)
WORKERS = 8
BATCH = 40                      # assets per store.write

ds = pl.read_parquet(
    "/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/"
    "7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad/ds2.parquet")
ASSETS = sorted(ds["asset"].unique().to_list())

done = {p.parent.parent.name.split("=", 1)[1]
        for p in DATA.glob(f"bars/asset=*/interval_s={INTERVAL}/*.parquet")}
todo = [a for a in ASSETS if a not in done]
print(f"{len(ASSETS)} assets in the traded set, {len(done)} already pulled, "
      f"{len(todo)} to go", flush=True)

src = BinanceArchive()
t0 = time.time()
fetched = 0


def one(asset):
    try:
        return asset, src.fetch_bars(asset, interval_s=INTERVAL, start=START, end=END)
    except Exception as exc:                      # a dead symbol is not an error
        return asset, None


try:
    for i in range(0, len(todo), BATCH):
        chunk = todo[i:i + BATCH]
        frames = []
        with ThreadPoolExecutor(max_workers=WORKERS) as pool:
            for asset, df in pool.map(one, chunk):
                if df is not None and df.height:
                    frames.append(df)
                    fetched += df.height
        if frames:
            combined = pl.concat(frames, how="vertical_relaxed")
            issues = store.write(combined, root=DATA, source=src.name)
            bad = [x for x in issues if x.severity.value != "info"]
            note = f"  {len(bad)} issue(s)" if bad else ""
            elapsed = time.time() - t0
            pace = (i + len(chunk)) / max(elapsed, 1e-9)
            left = (len(todo) - i - len(chunk)) / max(pace, 1e-9) / 60
            print(f"  {i+len(chunk):>4}/{len(todo)} assets  {fetched:>10,} bars  "
                  f"{elapsed/60:>5.1f}m elapsed  ~{left:.0f}m left{note}", flush=True)
finally:
    src.close()

print(f"\ndone: {fetched:,} hourly bars in {(time.time()-t0)/60:.1f} minutes")