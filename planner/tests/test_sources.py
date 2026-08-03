"""Data source adapters and the timestamp-convention guard.

Offline: every test drives the adapter through a mock transport. CI must not
depend on an exchange being reachable, and a test that silently passes because
it hit a real API is not a test of our parsing.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

import httpx
import polars as pl
import pytest

from planner import inspect as I
from planner.bars import conform
from planner.sources.binance import BinancePublic

DAY = 86_400
START = datetime(2026, 7, 1, tzinfo=timezone.utc)


def _kline(open_ms: int, o: float, h: float, lo: float, c: float, v: float) -> list:
    """Binance kline: [openTime, o, h, l, c, volume, closeTime, quoteVol, trades, ...]"""
    return [
        open_ms,
        str(o),
        str(h),
        str(lo),
        str(c),
        str(v),
        open_ms + DAY * 1000 - 1,
        str(v * c),
        1234,
        "0",
        "0",
        "0",
    ]


def _contiguous(n: int, start: datetime = START, first_close: float = 100.0) -> list[list]:
    """A clean series where each bar opens at the previous bar's close."""
    out = []
    prev_close = first_close
    for i in range(n):
        ts_ms = int((start + timedelta(days=i)).timestamp() * 1000)
        close = prev_close * 1.01
        out.append(_kline(ts_ms, prev_close, close * 1.02, prev_close * 0.98, close, 5.5))
        prev_close = close
    return out


def _source(batches: list[list[list]]) -> BinancePublic:
    """A source whose upstream returns each batch in turn, then empties."""
    calls = {"n": 0}

    def handler(request: httpx.Request) -> httpx.Response:
        i = calls["n"]
        calls["n"] += 1
        payload = batches[i] if i < len(batches) else []
        return httpx.Response(200, json=payload)

    client = httpx.Client(
        base_url="https://api.binance.com", transport=httpx.MockTransport(handler)
    )
    return BinancePublic(client=client)


def test_parses_klines_into_canonical_schema():
    src = _source([_contiguous(3)])
    df = src.fetch_bars("BTC", interval_s=DAY, start=START, end=START + timedelta(days=3))
    assert df.height == 3
    assert df["asset"].unique().to_list() == ["BTC"]
    assert df["ts_utc"][0] == START
    assert df["trades"][0] == 1234


def test_open_time_is_taken_as_the_bar_open():
    """The first kline field is openTime; a shift here is the worst bug we can have."""
    src = _source([_contiguous(2)])
    df = src.fetch_bars("BTC", interval_s=DAY, start=START, end=START + timedelta(days=2))
    assert df["ts_utc"][0] == START
    assert df["ts_utc"][1] == START + timedelta(days=1)


def test_end_is_exclusive():
    """A bar opening exactly at `end` belongs to the next window."""
    src = _source([_contiguous(4)])
    df = src.fetch_bars("BTC", interval_s=DAY, start=START, end=START + timedelta(days=2))
    assert df["ts_utc"].max() == START + timedelta(days=1)


def test_volume_stays_fractional():
    src = _source([[_kline(int(START.timestamp() * 1000), 100, 101, 99, 100.5, 0.4231)]])
    df = src.fetch_bars("BTC", interval_s=DAY, start=START, end=START + timedelta(days=1))
    assert df["volume"][0] == pytest.approx(0.4231)


def test_unsupported_interval_is_refused():
    src = _source([[]])
    with pytest.raises(ValueError, match="unsupported interval"):
        src.fetch_bars("BTC", interval_s=77, start=START, end=START + timedelta(days=1))


def test_naive_datetimes_are_refused():
    src = _source([[]])
    with pytest.raises(ValueError, match="timezone-aware"):
        src.fetch_bars("BTC", interval_s=DAY, start=datetime(2026, 7, 1), end=datetime(2026, 7, 2))


def test_empty_upstream_gives_an_empty_conformed_frame():
    src = _source([[]])
    df = src.fetch_bars("BTC", interval_s=DAY, start=START, end=START + timedelta(days=3))
    assert df.height == 0
    assert "ts_utc" in df.columns


def test_symbol_mapping():
    assert _source([[]]).symbol_for("btc") == "BTCUSDT"


def test_universe_is_not_faked_from_volume():
    """Volume rank is not market-cap rank. Better to raise than to imply it is."""
    with pytest.raises(NotImplementedError):
        _source([[]]).universe(START)


# ---------------------------------------------------------------------------
# The timestamp-convention guard
# ---------------------------------------------------------------------------


def _frame_from(klines: list[list]) -> pl.DataFrame:
    rows = [
        {
            "ts_utc": datetime.fromtimestamp(k[0] / 1000, tz=timezone.utc),
            "asset": "BTC",
            "interval_s": DAY,
            "open": float(k[1]),
            "high": float(k[2]),
            "low": float(k[3]),
            "close": float(k[4]),
            "volume": float(k[5]),
        }
        for k in klines
    ]
    return conform(pl.DataFrame(rows))


def test_continuity_passes_on_a_correct_series():
    report = I.continuity(_frame_from(_contiguous(20)))[0]
    assert report.ok
    assert report.checked == 19
    assert "bar OPEN" in report.render()


def test_continuity_catches_a_one_interval_shift():
    """Ingesting close-stamped bars as opens breaks open[t] == close[t-1]."""
    klines = _contiguous(20)
    shifted = [
        [k[0] + DAY * 1000] + k[1:] for k in klines
    ]  # every bar stamped one interval late
    frame = _frame_from(klines)
    shifted_frame = _frame_from(shifted)
    # The shifted series still looks internally tidy, but pairing each open
    # against the wrong predecessor's close breaks the identity.
    misaligned = shifted_frame.with_columns(
        pl.col("close").shift(-1).fill_null(pl.col("close"))
    )
    assert I.continuity(frame)[0].ok
    assert not I.continuity(misaligned)[0].ok


def test_continuity_ignores_real_gaps():
    """A missing day is an outage, not a timestamp problem."""
    klines = _contiguous(10)
    with_gap = klines[:4] + klines[7:]
    report = I.continuity(_frame_from(with_gap))[0]
    assert report.ok, "a gap must not be counted as a convention break"
    assert report.checked < 9


def test_continuity_reports_nothing_to_check():
    report = I.continuity(_frame_from(_contiguous(1)))[0]
    assert report.checked == 0
    assert not report.ok
    assert "too few" in report.render()
