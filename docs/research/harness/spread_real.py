"""Measure the real bid-ask spread from Binance futures bookTicker.

OHLC estimators failed their own validation - Corwin-Schultz reads 21bp on BTC
where the truth is about 1bp, and Abdi-Ranaldo floors at zero for every liquid
name - so the spread has to come from quotes. Binance publishes best bid/ask per
perpetual, which is the right instrument and the right venue type: this book
trades perps.

Full files are 35-188MB a symbol-day and there are 215 symbols over 2,500 days,
so this SAMPLES rather than backfills. A range request takes the first few MB and
the deflate stream is decompressed as far as it goes - a zip stores its entries
sequentially, so a prefix yields the first few hours of quotes, which is a fine
sample of a spread that does not change character within a day.

Time-weighted, not quote-weighted: a spread that is wide for one second and tight
for an hour should not count equally. What the book pays is the spread prevailing
when it trades, and trades are spread through time.
"""
import io, statistics, sys, zlib
import urllib.request
from pathlib import Path

B = "https://data.binance.vision/data/futures/um/daily/bookTicker"
# Spanning the liquidity range the strategy actually trades, plus BTC and ETH as
# validation anchors where the true spread is known.
SYMBOLS = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "DOGEUSDT", "AVAXUSDT",
           "LINKUSDT", "ARBUSDT", "GALAUSDT", "TRBUSDT", "CVCUSDT"]
DATES = ["2023-06-15", "2024-03-14", "2025-02-12", "2026-01-14"]
PREFIX_BYTES = 4_000_000


def sample(symbol, date):
    url = f"{B}/{symbol}/{symbol}-bookTicker-{date}.zip"
    req = urllib.request.Request(url, headers={"Range": f"bytes=0-{PREFIX_BYTES}"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            blob = r.read()
    except Exception:
        return None
    if len(blob) < 1000 or blob[:2] != b"PK":
        return None
    # Local file header: 30 fixed bytes, then filename and extra field.
    n_name = int.from_bytes(blob[26:28], "little")
    n_extra = int.from_bytes(blob[28:30], "little")
    start = 30 + n_name + n_extra
    d = zlib.decompressobj(-15)
    try:
        text = d.decompress(blob[start:]).decode("utf-8", errors="ignore")
    except Exception:
        return None

    rows = []
    for line in text.split("\n")[1:-1]:
        p = line.split(",")
        if len(p) < 5:
            continue
        try:
            bid, ask, ts = float(p[1]), float(p[3]), int(p[5] if len(p) > 5 else p[4])
        except (ValueError, IndexError):
            continue
        if bid > 0 and ask > bid:
            rows.append((ts, (ask - bid) / ((ask + bid) / 2)))
    if len(rows) < 500:
        return None
    rows.sort()
    # Time-weight each quote by how long it stood.
    num = den = 0.0
    for i in range(len(rows) - 1):
        dt = rows[i + 1][0] - rows[i][0]
        if 0 < dt < 60_000:
            num += rows[i][1] * dt
            den += dt
    if den <= 0:
        return None
    return num / den, len(rows)


print(f"{'symbol':<10}" + "".join(f"{d:>13}" for d in DATES) + f"{'median':>10}")
out = {}
for sym in SYMBOLS:
    vals = []
    cells = ""
    for d in DATES:
        r = sample(sym, d)
        if r is None:
            cells += f"{'-':>13}"
        else:
            s, n = r
            vals.append(s)
            cells += f"{s*10000:>12.2f}b"
        sys.stdout.flush()
    med = statistics.median(vals) if vals else None
    out[sym] = med
    print(f"{sym:<10}{cells}{(f'{med*10000:.2f}b' if med else '-'):>10}")

print("\nvalidation: BTC and ETH true spreads are ~0.5-1bp on perps")
for s in ("BTCUSDT", "ETHUSDT"):
    if out.get(s):
        print(f"  {s}: {out[s]*10000:.2f}bp full / {out[s]*10000/2:.2f}bp half")

good = {k: v for k, v in out.items() if v}
if good:
    vals = sorted(good.values())
    print(f"\nfull spread across the sample: median {statistics.median(vals)*10000:.2f}bp, "
          f"max {max(vals)*10000:.2f}bp")
    print(f"HALF spread (what a taker crosses): "
          f"median {statistics.median(vals)*10000/2:.2f}bp, max {max(vals)*10000/2:.2f}bp")
    print(f"\nconfig assumes 2.0bp half-spread.")