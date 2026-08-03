"""Binance public market data.

Public REST, no account, no key. Chosen for Phase 0 because it has the broadest
liquid-spot coverage and is the de-facto reference series for crypto OHLCV, and
because needing no credentials is what keeps the venue decision open.

**This is a data choice, not a venue choice.** Nothing here places an order or
knows how to. Trading on OKX or Hyperliquid while pricing from this source is
expected.

The timestamp convention, which is the thing worth getting right:

    Binance klines return openTime first and closeTime seventh.
    openTime IS the bar open, in epoch milliseconds UTC.

So the conversion is a unit change, not a shift. If a future source stamps
close times instead, it shifts at *its* boundary - never here, never downstream.
"""

from __future__ import annotations

import time
from datetime import datetime, timedelta, timezone

import httpx
import polars as pl

from ..bars import conform
from .base import UniverseMember

_BASE = "https://api.binance.com"
_MAX_LIMIT = 1000  # klines per request, Binance's cap

_INTERVAL_NAMES: dict[int, str] = {
    60: "1m",
    300: "5m",
    900: "15m",
    3_600: "1h",
    14_400: "4h",
    86_400: "1d",
}

# Canonical asset -> quote pair. Kept explicit rather than string-concatenated
# so an asset whose USDT pair does not exist fails loudly at config time.
_QUOTE = "USDT"


class BinancePublic:
    """Read-only public data source."""

    name = "binance_public"

    def __init__(self, *, timeout_s: float = 20.0, client: httpx.Client | None = None) -> None:
        self._client = client or httpx.Client(
            base_url=_BASE,
            timeout=timeout_s,
            headers={"User-Agent": "ai-trader/0.1 (public market data)"},
        )

    def supported_intervals(self) -> list[int]:
        return sorted(_INTERVAL_NAMES)

    def symbol_for(self, asset: str) -> str:
        return f"{asset.upper()}{_QUOTE}"

    def fetch_bars(
        self,
        asset: str,
        *,
        interval_s: int,
        start: datetime,
        end: datetime,
    ) -> pl.DataFrame:
        if interval_s not in _INTERVAL_NAMES:
            raise ValueError(
                f"unsupported interval {interval_s}s; have {sorted(_INTERVAL_NAMES)}"
            )
        if start.tzinfo is None or end.tzinfo is None:
            raise ValueError("start and end must be timezone-aware")

        rows: list[dict] = []
        cursor = start
        step = timedelta(seconds=interval_s)

        while cursor < end:
            batch = self._klines(
                symbol=self.symbol_for(asset),
                interval=_INTERVAL_NAMES[interval_s],
                start_ms=int(cursor.timestamp() * 1000),
                end_ms=int(end.timestamp() * 1000),
            )
            if not batch:
                break

            for k in batch:
                open_ms = int(k[0])
                rows.append(
                    {
                        "ts_utc": datetime.fromtimestamp(open_ms / 1000, tz=timezone.utc),
                        "asset": asset.upper(),
                        "interval_s": interval_s,
                        "open": float(k[1]),
                        "high": float(k[2]),
                        "low": float(k[3]),
                        "close": float(k[4]),
                        "volume": float(k[5]),
                        "quote_volume": float(k[7]),
                        "trades": int(k[8]),
                    }
                )

            last_open = datetime.fromtimestamp(int(batch[-1][0]) / 1000, tz=timezone.utc)
            advanced = last_open + step
            if advanced <= cursor:  # no forward progress; stop rather than spin
                break
            cursor = advanced

            if len(batch) < _MAX_LIMIT:
                break
            time.sleep(0.15)  # stay well inside the public weight budget

        if not rows:
            from ..bars import empty_frame

            return empty_frame()

        df = conform(pl.DataFrame(rows))
        # `end` is exclusive: a bar opening exactly at `end` belongs to the next
        # window, and including it would hand the caller a bar past its horizon.
        return df.filter(pl.col("ts_utc") < end)

    def _klines(self, *, symbol: str, interval: str, start_ms: int, end_ms: int) -> list[list]:
        last_error: Exception | None = None
        for attempt in range(3):
            try:
                r = self._client.get(
                    "/api/v3/klines",
                    params={
                        "symbol": symbol,
                        "interval": interval,
                        "startTime": start_ms,
                        "endTime": end_ms,
                        "limit": _MAX_LIMIT,
                    },
                )
                if r.status_code == 400:
                    raise ValueError(f"{symbol}: rejected by upstream - {r.text[:200]}")
                r.raise_for_status()
                return r.json()
            except (httpx.HTTPError, ValueError) as exc:
                last_error = exc
                if isinstance(exc, ValueError):
                    raise
                time.sleep(1.5 * (attempt + 1))
        raise RuntimeError(f"{symbol}: data fetch failed after 3 attempts") from last_error

    def universe(self, as_of: datetime) -> list[UniverseMember]:
        """Not implemented here, deliberately.

        Binance's public endpoints expose traded volume, not market
        capitalisation, so a rank derived from them is a liquidity rank wearing
        a market-cap label. Universe construction is a Phase 1 decision that
        needs a real ranking source and a point-in-time snapshot discipline;
        until then `universe.py` reads an explicit configured list and records
        it as an observation.
        """
        raise NotImplementedError(
            "universe ranking is Phase 1 - see planner.universe for the configured snapshot"
        )

    def close(self) -> None:
        self._client.close()
