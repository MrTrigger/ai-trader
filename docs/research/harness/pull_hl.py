"""Pull Hyperliquid funding history, politely.

Someone else's public endpoint, so: sequential requests, a real gap between
them, and exponential backoff on 429 rather than retrying immediately. Five
assets rather than forty - enough to establish whether the Binance proxy tracks,
and the answer is going to be dominated by the pre-2023 period where Hyperliquid
has no data at all.
"""
import json, time, urllib.request, urllib.error, datetime as dt

ASSETS = ["BTC", "ETH", "SOL", "DOGE", "AVAX"]
START = int(dt.datetime(2023, 5, 1, tzinfo=dt.timezone.utc).timestamp() * 1000)
END = int(dt.datetime(2026, 8, 1, tzinfo=dt.timezone.utc).timestamp() * 1000)


def post(body, tries=6):
    delay = 1.0
    for attempt in range(tries):
        try:
            req = urllib.request.Request(
                "https://api.hyperliquid.xyz/info",
                data=json.dumps(body).encode(),
                headers={"Content-Type": "application/json"},
            )
            with urllib.request.urlopen(req, timeout=40) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            if e.code != 429 or attempt == tries - 1:
                raise
            time.sleep(delay)
            delay *= 2
    return []


out = {}
for coin in ASSETS:
    rows, cursor, calls = [], START, 0
    while cursor < END and calls < 400:
        got = post({"type": "fundingHistory", "coin": coin,
                    "startTime": cursor, "endTime": END})
        calls += 1
        if not got:
            break
        rows.extend(got)
        last = got[-1]["time"]
        if last <= cursor:
            break
        cursor = last + 1
        time.sleep(0.55)
        if len(got) < 500:
            break
    out[coin] = [(r["time"], float(r["fundingRate"])) for r in rows]
    if rows:
        a = dt.datetime.fromtimestamp(rows[0]["time"] / 1000, dt.timezone.utc).date()
        b = dt.datetime.fromtimestamp(rows[-1]["time"] / 1000, dt.timezone.utc).date()
        print(f"  {coin:<6} {len(rows):>6} records  {a} -> {b}  ({calls} calls)", flush=True)
    else:
        print(f"  {coin:<6} no data", flush=True)

json.dump(out, open("hl_funding.json", "w"))
print("wrote hl_funding.json")