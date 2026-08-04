"""Binance's public data archive — the source that reaches the dead.

`data.binance.vision` publishes monthly OHLCV dumps as static zips. Two things
make it worth having alongside the REST source, and the second is the one that
matters:

**It is much faster for bulk history.** One HTTP request per symbol-month
instead of one per thousand bars.

**It retains delisted symbols.** The REST `exchangeInfo` endpoint enumerates 682
USDT pairs of which 203 are `BREAK` — already delisted — and the archive
enumerates 723, including symbols purged from the API entirely. That difference
is the whole reason this module exists.

## Why that matters more than it sounds

A universe reconstructed from *currently trading* symbols is a universe selected
for having survived, and applying it to history is survivorship bias (§12). It
does not bite evenly: it bites momentum hardest, because the assets a momentum
strategy would have bought are disproportionately the ones that ran up and then
died. A backtest on survivors would make such a strategy look good precisely
*because* it is the wrong strategy.

ANC is the worked example. Anchor Protocol went with Terra in May 2022, and the
archive still serves every bar of it:

    2022-03-01  close 3.738
    2022-05-01  close 1.683  ->  2022-05-31  close 0.234
    2022-06-30  close 0.144

Being able to hold that in a backtest is the difference between measuring a
strategy and measuring the survivors of one.

## The distinction this module relies on

Reconstructing a **rule** from complete history is point-in-time. Reconstructing
a **survivor list** is not. "Top N by trailing median quote volume among symbols
with bars at T" uses only inputs knowable at T, so computing it today for a past
date is legitimate. Downloading today's top-N list and applying it to 2024 is
not, and no amount of care makes it so.

## Timestamp convention, and the trap in it

Archive columns are positional and match the REST kline array: `open_time`
first, and it **is** the bar open — same convention as `binance.py`, converted
at the same boundary and never shifted downstream (§12).

**The unit is not constant across the archive.** Older dumps stamp epoch
*milliseconds*; newer ones stamp *microseconds*. Assuming milliseconds
throughout puts recent bars in the year 57133, which at least fails loudly —
but the same class of mistake in the other direction would put them in 1970,
and a 1970 bar silently sorts before everything and quietly becomes the oldest
row in a rolling window. So `_epoch_utc` infers the unit by magnitude and then
**range-checks the result**, refusing anything outside a plausible trading era
rather than producing a date that is merely arithmetic.

Newer files also carry a header row; older ones do not, and both are handled.
"""

from __future__ import annotations

import csv
import io
import re
import urllib.parse
import urllib.request
import zipfile
from datetime import date, datetime, timedelta, timezone

import polars as pl

from ..bars import conform, empty_frame
from .base import UniverseMember

_ARCHIVE = "https://data.binance.vision"
_LISTING = "https://s3-ap-northeast-1.amazonaws.com/data.binance.vision"
_QUOTE = "USDT"

_INTERVAL_NAMES: dict[int, str] = {
    60: "1m",
    300: "5m",
    900: "15m",
    3_600: "1h",
    14_400: "4h",
    86_400: "1d",
}

#: Binance's leveraged tokens. Enumerated by the archive and correctly excluded:
#: they are derivative products with embedded rebalancing, not assets, and a
#: momentum rank over them is a rank over their leverage rather than their
#: underlying.
_LEVERAGED = re.compile(r"(UP|DOWN|BEAR|BULL)$")


class BinanceArchive:
    """Bulk historical bars from the public archive.

    Read-only, no account, no key — same posture as `BinancePublic`, and
    likewise a *data* choice rather than a venue choice.
    """

    name = "binance_archive"

    def __init__(self, *, timeout_s: float = 60.0, opener=None) -> None:
        self._timeout = timeout_s
        self._open = opener or urllib.request.urlopen

    def supported_intervals(self) -> list[int]:
        return sorted(_INTERVAL_NAMES)

    def symbol_for(self, asset: str) -> str:
        return f"{asset.upper()}{_QUOTE}"

    # --- bars --------------------------------------------------------------

    def fetch_bars(
        self,
        asset: str,
        *,
        interval_s: int,
        start: datetime,
        end: datetime,
    ) -> pl.DataFrame:
        """Bars in `[start, end)`, conformed to the canonical schema.

        A month with no file is skipped rather than raising: for a delisted
        symbol every month after its last is legitimately absent, and that
        absence is the delisting. Treating it as an error would make the dead
        unrepresentable, which is the bias this module exists to remove.
        """
        if interval_s not in _INTERVAL_NAMES:
            raise ValueError(
                f"unsupported interval {interval_s}s; have {sorted(_INTERVAL_NAMES)}"
            )
        if start.tzinfo is None or end.tzinfo is None:
            raise ValueError("start and end must be timezone-aware")

        rows: list[dict] = []
        for month in _months(start, end):
            rows.extend(self._month(asset, interval_s=interval_s, month=month))

        if not rows:
            return empty_frame()

        df = conform(pl.DataFrame(rows))
        # `end` exclusive, matching the REST source: a bar opening exactly at
        # `end` belongs to the next window.
        return df.filter((pl.col("ts_utc") >= start) & (pl.col("ts_utc") < end))

    def _month(self, asset: str, *, interval_s: int, month: str) -> list[dict]:
        symbol = self.symbol_for(asset)
        name = _INTERVAL_NAMES[interval_s]
        url = f"{_ARCHIVE}/data/spot/monthly/klines/{symbol}/{name}/{symbol}-{name}-{month}.zip"

        try:
            payload = self._open(url, timeout=self._timeout).read()
        except Exception:
            return []

        out: list[dict] = []
        with zipfile.ZipFile(io.BytesIO(payload)) as archive:
            with archive.open(archive.namelist()[0]) as handle:
                for record in csv.reader(io.TextIOWrapper(handle, encoding="utf-8")):
                    # Newer dumps carry a header row, older ones do not.
                    if not record or not record[0].isdigit():
                        continue
                    out.append(
                        {
                            "ts_utc": _epoch_utc(record[0], context=f"{symbol} {month}"),
                            "asset": asset.upper(),
                            "interval_s": interval_s,
                            "open": float(record[1]),
                            "high": float(record[2]),
                            "low": float(record[3]),
                            "close": float(record[4]),
                            "volume": float(record[5]),
                            "quote_volume": float(record[7]),
                            "trades": int(record[8]),
                        }
                    )
        return out

    # --- what exists at all ------------------------------------------------

    def listed_assets(self, *, include_leveraged: bool = False) -> list[str]:
        """Every asset the archive has ever carried a USDT pair for.

        Including the dead. This is the input to a point-in-time universe rule,
        and using `exchangeInfo` instead would silently restrict it to survivors.
        """
        assets = []
        for symbol in sorted(self._symbols()):
            if not symbol.endswith(_QUOTE):
                continue
            asset = symbol[: -len(_QUOTE)]
            if not asset:
                continue
            if not include_leveraged and _LEVERAGED.search(asset):
                continue
            assets.append(asset)
        return assets

    def _symbols(self) -> set[str]:
        prefix = "data/spot/monthly/klines/"
        found: set[str] = set()
        marker = ""

        while True:
            query = urllib.parse.urlencode(
                {"delimiter": "/", "prefix": prefix, "max-keys": "1000", "marker": marker}
            )
            body = self._open(f"{_LISTING}?{query}", timeout=self._timeout).read().decode()
            found.update(
                re.findall(rf"<Prefix>{re.escape(prefix)}([^<]+)/</Prefix>", body)
            )

            if "<IsTruncated>true</IsTruncated>" not in body:
                return found
            match = re.search(r"<NextMarker>([^<]+)</NextMarker>", body)
            if not match:
                return found
            marker = match.group(1)

    def universe(self, as_of: datetime) -> list[UniverseMember]:
        """Not here — ranking is `universe.by_liquidity` over stored bars.

        Deliberate: a universe rule must be computable from the same bars the
        backtest replays, or the live ranking and the historical one are two
        different rules. A source-side ranking could only ever describe today.
        """
        raise NotImplementedError(
            "ranking lives in planner.universe.by_liquidity, over the bar store"
        )


#: The window a bar timestamp may plausibly fall in. Binance opened in 2017, and
#: anything past the near future is a unit error rather than a bar.
_EARLIEST = datetime(2015, 1, 1, tzinfo=timezone.utc)
_LATEST = datetime(2100, 1, 1, tzinfo=timezone.utc)


def _epoch_utc(raw: str, *, context: str = "") -> datetime:
    """Epoch integer to UTC, inferring the unit and refusing implausible results.

    The archive is not internally consistent: older dumps are in milliseconds
    and newer ones in microseconds. Inferring by magnitude is safe because the
    ranges are three orders of magnitude apart and no plausible date is
    ambiguous between them.

    The range check is the part that matters. A wrong unit is a *silent* bug in
    one direction — seconds read as milliseconds lands in 1970, which sorts
    before every real bar and becomes the oldest row of every rolling window
    without anything looking wrong. Refusing here means the failure is the
    import, not the strategy.
    """
    value = int(raw)
    if value >= 1e15:
        seconds = value / 1_000_000  # microseconds
    elif value >= 1e12:
        seconds = value / 1_000  # milliseconds
    elif value >= 1e9:
        seconds = float(value)  # seconds
    else:
        raise ValueError(
            f"{context}: timestamp {value} is too small to be an epoch in any unit"
        )

    try:
        stamped = datetime.fromtimestamp(seconds, tz=timezone.utc)
    except (ValueError, OverflowError, OSError) as exc:
        # Far enough out that the conversion itself gives up. Same diagnosis as
        # the range check below, and it should read the same way.
        raise ValueError(
            f"{context}: timestamp {value} is not a date in any unit ({exc})"
        ) from exc

    if not _EARLIEST <= stamped <= _LATEST:
        raise ValueError(
            f"{context}: timestamp {value} resolves to {stamped.isoformat()}, outside "
            "any plausible trading era. This is a unit error, not a bar."
        )
    return stamped


def _months(start: datetime, end: datetime) -> list[str]:
    """`YYYY-MM` strings covering `[start, end)`, inclusive of both ends' months."""
    out: list[str] = []
    cursor = date(start.year, start.month, 1)
    last = date(end.year, end.month, 1)
    while cursor <= last:
        out.append(f"{cursor.year:04d}-{cursor.month:02d}")
        cursor = date(cursor.year + (cursor.month == 12), (cursor.month % 12) + 1, 1)
    return out


def is_leveraged_token(asset: str) -> bool:
    return bool(_LEVERAGED.search(asset.upper()))


__all__ = ["BinanceArchive", "is_leveraged_token"]
