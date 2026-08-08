"""Pull 2017-2021 bars: a window with NO overlap with anything tested so far.

Binance USD-M perps began Sept 2019, so the usable fresh window with funding is
2019-10 -> 2021-09. Bars are pulled from 2017 so the 144-bar channel warmup and
the 90-bar history floor are satisfied before the first decision.
"""
import json, sys, time, urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path
import polars as pl
from planner import store
from planner.sources import BinancePublic, is_leveraged_token

ROOT = Path(sys.argv[1])
START = datetime(2017, 1, 1, tzinfo=timezone.utc)
END = datetime(2021, 10, 2, tzinfo=timezone.utc)

info = json.loads(urllib.request.urlopen(
    "https://api.binance.com/api/v3/exchangeInfo?permissions=SPOT", timeout=60).read())
assets = sorted({s["baseAsset"] for s in info["symbols"]
                 if s["quoteAsset"] == "USDT" and not is_leveraged_token(s["baseAsset"])})
print(f"{len(assets)} assets", flush=True)

def fetch(a):
    src = BinancePublic()
    try:
        return a, src.fetch_bars(a, interval_s=86400, start=START, end=END), None
    except Exception as e:
        return a, None, e
    finally:
        src.close()

frames, done, t0 = [], 0, time.time()
with ThreadPoolExecutor(max_workers=8) as pool:
    for fut in as_completed([pool.submit(fetch, a) for a in assets]):
        a, df, err = fut.result(); done += 1
        if df is not None and not df.is_empty(): frames.append(df)
        if done % 100 == 0: print(f"  {done}/{len(assets)} {time.time()-t0:.0f}s", flush=True)

combined = pl.concat(frames)
print(f"{len(frames)} assets with pre-2021-10 bars, {combined.height} rows", flush=True)
issues = store.write(combined, root=ROOT, source="binance_public")
print(f"errors: {[i for i in issues if i.severity.value=='error']}", flush=True)
print("DONE", flush=True)