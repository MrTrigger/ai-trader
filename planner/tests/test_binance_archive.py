"""The archive source — offline.

Every test here builds its own zips rather than reaching the network, so the
suite stays deterministic and runnable without it. The one thing that *cannot*
be checked offline — that the archive agrees with the REST source bar for bar —
is checked by `test_sources.py`'s live comparison when it is run.

The timestamp handling is over-represented on purpose. Binance's archive is not
internally consistent about its epoch unit, and a wrong unit is the class of bug
this codebase treats as its most dangerous (§12): silent, and it shifts every
bar.
"""

from __future__ import annotations

import io
import zipfile
from datetime import datetime, timedelta, timezone

import pytest

from planner.sources.binance_archive import (
    BinanceArchive,
    _epoch_utc,
    _months,
    is_leveraged_token,
)

UTC = timezone.utc


def kline_row(open_ms: int, *, unit: str = "ms", close: float = 100.0) -> list:
    """One row in Binance's positional kline layout."""
    stamp = open_ms * 1000 if unit == "us" else open_ms
    return [
        str(stamp),
        "99.0",   # open
        "101.0",  # high
        "98.0",   # low
        str(close),
        "1000.0",       # volume
        str(stamp + 1), # close_time
        "123456.0",     # quote_volume
        "42",           # trades
        "0", "0", "0",
    ]


def zip_of(rows: list[list], *, header: bool = False) -> bytes:
    body = io.StringIO()
    if header:
        body.write("open_time,open,high,low,close,volume,close_time,quote_volume,count,a,b,c\n")
    for row in rows:
        body.write(",".join(row) + "\n")

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as archive:
        archive.writestr("data.csv", body.getvalue())
    return buf.getvalue()


class FakeResponse:
    def __init__(self, payload: bytes) -> None:
        self._payload = payload

    def read(self) -> bytes:
        return self._payload


def opener_for(files: dict[str, bytes]):
    """Serve exact URLs; anything else 404s, as a missing month does."""

    def _open(url, timeout=None):
        for fragment, payload in files.items():
            if fragment in url:
                return FakeResponse(payload)
        raise OSError(f"404 {url}")

    return _open


DAY_MS = 86_400_000
JAN_1_2022 = 1_640_995_200_000  # 2022-01-01T00:00:00Z


# --- timestamps ------------------------------------------------------------


def test_milliseconds_are_recognised():
    assert _epoch_utc(str(JAN_1_2022)) == datetime(2022, 1, 1, tzinfo=UTC)


def test_microseconds_are_recognised():
    # Newer dumps switched units. Assuming milliseconds here puts the bar in
    # the year 57133; assuming microseconds for a millisecond file puts it in
    # 1971, which sorts before every real bar and silently poisons every
    # rolling window.
    assert _epoch_utc(str(JAN_1_2022 * 1000)) == datetime(2022, 1, 1, tzinfo=UTC)


def test_seconds_are_recognised():
    assert _epoch_utc(str(JAN_1_2022 // 1000)) == datetime(2022, 1, 1, tzinfo=UTC)


def test_an_implausible_date_is_refused_rather_than_computed():
    # Just past the microsecond range: arithmetic succeeds and produces a bar
    # in the year 3970, which is the case a range check has to catch.
    with pytest.raises(ValueError, match="outside any plausible trading era"):
        _epoch_utc(str(63_000_000_000_000_000), context="X 2022-01")


def test_a_timestamp_too_large_to_convert_at_all_reads_the_same_way():
    with pytest.raises(ValueError, match="not a date in any unit"):
        _epoch_utc(str(JAN_1_2022 * 1_000_000), context="X 2022-01")


def test_a_value_too_small_for_any_unit_is_refused():
    with pytest.raises(ValueError, match="too small to be an epoch"):
        _epoch_utc("12345")


def test_the_refusal_names_what_was_being_read():
    with pytest.raises(ValueError, match="ANCUSDT 2022-05"):
        _epoch_utc("1", context="ANCUSDT 2022-05")


# --- bars ------------------------------------------------------------------


def test_bars_parse_from_a_monthly_zip():
    rows = [kline_row(JAN_1_2022 + i * DAY_MS, close=100.0 + i) for i in range(5)]
    source = BinanceArchive(opener=opener_for({"BTCUSDT-1d-2022-01.zip": zip_of(rows)}))

    df = source.fetch_bars(
        "BTC",
        interval_s=86_400,
        start=datetime(2022, 1, 1, tzinfo=UTC),
        end=datetime(2022, 2, 1, tzinfo=UTC),
    )
    assert df.height == 5
    assert df["asset"].unique().to_list() == ["BTC"]
    assert df["ts_utc"][0] == datetime(2022, 1, 1, tzinfo=UTC)
    assert df["close"][4] == 104.0
    assert df["quote_volume"][0] == 123456.0


def test_a_header_row_is_skipped():
    rows = [kline_row(JAN_1_2022)]
    source = BinanceArchive(
        opener=opener_for({"BTCUSDT-1d-2022-01.zip": zip_of(rows, header=True)})
    )
    df = source.fetch_bars(
        "BTC",
        interval_s=86_400,
        start=datetime(2022, 1, 1, tzinfo=UTC),
        end=datetime(2022, 2, 1, tzinfo=UTC),
    )
    assert df.height == 1


def test_both_epoch_units_land_on_the_same_bar():
    ms = zip_of([kline_row(JAN_1_2022, unit="ms")])
    us = zip_of([kline_row(JAN_1_2022, unit="us")])
    window = dict(
        interval_s=86_400,
        start=datetime(2022, 1, 1, tzinfo=UTC),
        end=datetime(2022, 2, 1, tzinfo=UTC),
    )
    a = BinanceArchive(opener=opener_for({"2022-01.zip": ms})).fetch_bars("BTC", **window)
    b = BinanceArchive(opener=opener_for({"2022-01.zip": us})).fetch_bars("BTC", **window)
    assert a.equals(b)


def test_a_missing_month_is_a_delisting_not_an_error():
    """Every month after a dead symbol's last is legitimately absent.

    Raising would make the delisted unrepresentable, which is the bias this
    source exists to remove.
    """
    rows = [kline_row(JAN_1_2022 + i * DAY_MS) for i in range(3)]
    source = BinanceArchive(opener=opener_for({"ANCUSDT-1d-2022-01.zip": zip_of(rows)}))

    df = source.fetch_bars(
        "ANC",
        interval_s=86_400,
        start=datetime(2022, 1, 1, tzinfo=UTC),
        end=datetime(2022, 4, 1, tzinfo=UTC),  # Feb and Mar do not exist
    )
    assert df.height == 3
    assert df["ts_utc"].max() == datetime(2022, 1, 3, tzinfo=UTC)


def test_a_symbol_with_no_data_at_all_is_an_empty_frame():
    source = BinanceArchive(opener=opener_for({}))
    df = source.fetch_bars(
        "GHOST",
        interval_s=86_400,
        start=datetime(2022, 1, 1, tzinfo=UTC),
        end=datetime(2022, 2, 1, tzinfo=UTC),
    )
    assert df.is_empty()


def test_the_end_of_the_window_is_exclusive():
    # Matching the REST source: a bar opening exactly at `end` belongs to the
    # next window, and including it hands the caller a bar past its horizon.
    rows = [kline_row(JAN_1_2022 + i * DAY_MS) for i in range(10)]
    source = BinanceArchive(opener=opener_for({"BTCUSDT-1d-2022-01.zip": zip_of(rows)}))
    df = source.fetch_bars(
        "BTC",
        interval_s=86_400,
        start=datetime(2022, 1, 1, tzinfo=UTC),
        end=datetime(2022, 1, 5, tzinfo=UTC),
    )
    assert df["ts_utc"].max() == datetime(2022, 1, 4, tzinfo=UTC)


def test_an_unsupported_interval_is_refused():
    with pytest.raises(ValueError, match="unsupported interval"):
        BinanceArchive(opener=opener_for({})).fetch_bars(
            "BTC",
            interval_s=7,
            start=datetime(2022, 1, 1, tzinfo=UTC),
            end=datetime(2022, 2, 1, tzinfo=UTC),
        )


def test_naive_datetimes_are_refused():
    with pytest.raises(ValueError, match="timezone-aware"):
        BinanceArchive(opener=opener_for({})).fetch_bars(
            "BTC", interval_s=86_400, start=datetime(2022, 1, 1), end=datetime(2022, 2, 1)
        )


# --- month enumeration -----------------------------------------------------


def test_months_cover_both_ends():
    got = _months(datetime(2022, 11, 15, tzinfo=UTC), datetime(2023, 2, 3, tzinfo=UTC))
    assert got == ["2022-11", "2022-12", "2023-01", "2023-02"]


def test_a_single_month_window_is_one_month():
    assert _months(
        datetime(2022, 5, 2, tzinfo=UTC), datetime(2022, 5, 28, tzinfo=UTC)
    ) == ["2022-05"]


# --- what counts as an asset -----------------------------------------------


def test_leveraged_tokens_are_recognised():
    # Derivative products with embedded rebalancing, not assets. A momentum
    # rank over them ranks their leverage rather than their underlying.
    for token in ("BTCUP", "BTCDOWN", "ETHBULL", "ETHBEAR"):
        assert is_leveraged_token(token)
    for asset in ("BTC", "ETH", "SOL", "UNI", "DOT"):
        assert not is_leveraged_token(asset)


def test_listed_assets_excludes_leveraged_tokens_by_default(monkeypatch):
    source = BinanceArchive(opener=opener_for({}))
    monkeypatch.setattr(
        source, "_symbols", lambda: {"BTCUSDT", "ANCUSDT", "BTCUPUSDT", "ETHBULLUSDT", "ETHBTC"}
    )
    assert source.listed_assets() == ["ANC", "BTC"]
    assert "BTCUP" in source.listed_assets(include_leveraged=True)


def test_listed_assets_includes_the_dead(monkeypatch):
    """The whole point: a universe built from survivors is survivorship bias."""
    source = BinanceArchive(opener=opener_for({}))
    monkeypatch.setattr(source, "_symbols", lambda: {"BTCUSDT", "ANCUSDT", "LUNAUSDT"})
    assert "ANC" in source.listed_assets()


def test_ranking_is_not_offered_by_the_source(monkeypatch):
    # A source-side ranking could only ever describe today. The rule must be
    # computable from the same bars the backtest replays, or live and history
    # are two different rules.
    with pytest.raises(NotImplementedError, match="by_liquidity"):
        BinanceArchive(opener=opener_for({})).universe(datetime(2022, 1, 1, tzinfo=UTC))
