"""Bar schema and store.

The properties worth guarding here are the ones that produce silent wrongness:
a bar that got written despite failing validation, a re-pull that grew a second
truth for the same timestamp, and a read that returned a bar newer than the
horizon it was given.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

import polars as pl
import pytest

from planner import bars as B
from planner import store as S

DAY = 86_400


def _frame(rows: list[dict]) -> pl.DataFrame:
    return pl.DataFrame(rows).with_columns(
        pl.col("ts_utc").dt.replace_time_zone("UTC"),
        pl.col("interval_s").cast(pl.Int32),
    )


def _bar(day: int, *, asset="BTC", close=100.0, volume=1.5, interval_s=DAY) -> dict:
    return {
        "ts_utc": datetime(2026, 7, day),
        "asset": asset,
        "interval_s": interval_s,
        "open": close - 1,
        "high": close + 2,
        "low": close - 2,
        "close": close,
        "volume": volume,
    }


def test_conform_fills_nullable_columns():
    df = B.conform(_frame([_bar(1)]))
    assert df.columns == B.BAR_COLUMNS
    assert df["quote_volume"].null_count() == 1
    assert df["trades"].null_count() == 1


def test_conform_refuses_a_missing_required_column():
    df = _frame([_bar(1)]).drop("close")
    with pytest.raises(ValueError, match="missing required columns"):
        B.conform(df)


def test_volume_is_fractional():
    """Crypto volume is not an integer. Rounding it destroys impact estimates."""
    df = B.conform(_frame([_bar(1, volume=0.4231)]))
    assert df["volume"][0] == pytest.approx(0.4231)


def test_validate_flags_inconsistent_ohlc():
    bad = _bar(1)
    bad["high"] = bad["low"] - 1
    issues = B.validate(B.conform(_frame([bad])))
    assert B.has_errors(issues)
    assert any(i.code == "ohlc_inconsistent" for i in issues)


def test_validate_flags_duplicate_bars():
    issues = B.validate(B.conform(_frame([_bar(1), _bar(1)])))
    assert B.has_errors(issues)
    assert any(i.code == "duplicate_bar" for i in issues)


def test_validate_flags_nonpositive_price():
    bad = _bar(1, close=0.0)
    bad["low"] = 0.0
    bad["open"] = 0.0
    issues = B.validate(B.conform(_frame([bad])))
    assert any(i.code == "nonpositive_price" for i in issues)


def test_gaps_are_a_warning_not_an_error():
    issues = B.validate(B.conform(_frame([_bar(1), _bar(5)])))
    assert not B.has_errors(issues)
    assert any(i.code == "gap" for i in issues)


def test_write_refuses_bad_data_entirely(tmp_path):
    bad = _bar(1)
    bad["high"] = bad["low"] - 1
    with pytest.raises(ValueError, match="refusing to write"):
        S.write(_frame([bad]), root=tmp_path, source="test")
    assert not (tmp_path / "bars" / "asset=BTC").exists()


def test_write_then_read(tmp_path):
    S.write(_frame([_bar(1), _bar(2), _bar(3)]), root=tmp_path, source="test")
    df = S.read(root=tmp_path, interval_s=DAY)
    assert df.height == 3
    assert df["asset"].unique().to_list() == ["BTC"]


def test_repull_deduplicates_and_prefers_newer(tmp_path):
    S.write(_frame([_bar(1, close=100.0)]), root=tmp_path, source="first")
    S.write(_frame([_bar(1, close=111.0)]), root=tmp_path, source="correction")
    df = S.read(root=tmp_path, interval_s=DAY)
    assert df.height == 1, "a re-pull must not grow a second truth for the same bar"
    assert df["close"][0] == 111.0


def test_read_respects_the_horizon(tmp_path):
    S.write(_frame([_bar(1), _bar(2), _bar(3)]), root=tmp_path, source="test")
    df = S.read(root=tmp_path, interval_s=DAY, until=datetime(2026, 7, 2, tzinfo=timezone.utc))
    assert df.height == 2
    assert df["ts_utc"].max() == datetime(2026, 7, 2, tzinfo=timezone.utc)


def test_content_hash_is_order_independent(tmp_path):
    a = B.conform(_frame([_bar(1), _bar(2)]))
    b = B.conform(_frame([_bar(2), _bar(1)]))
    assert S.content_hash(a) == S.content_hash(b)


def test_content_hash_changes_with_content():
    a = B.conform(_frame([_bar(1, close=100.0)]))
    b = B.conform(_frame([_bar(1, close=101.0)]))
    assert S.content_hash(a) != S.content_hash(b)


def test_daily_bars_partition_by_year():
    key = S.partition_key(datetime(2026, 7, 4, tzinfo=timezone.utc), DAY)
    assert key == "2026"


def test_intraday_bars_partition_by_month():
    key = S.partition_key(datetime(2026, 7, 4, tzinfo=timezone.utc), 3600)
    assert key == "2026-07"


def test_manifest_records_source_and_hash(tmp_path):
    S.write(_frame([_bar(1)]), root=tmp_path, source="unit-test")
    manifests = list((tmp_path / "bars" / "_manifests").glob("*.json"))
    assert len(manifests) == 1
    import json

    m = json.loads(manifests[0].read_text())
    assert m["source"] == "unit-test"
    assert m["asset"] == "BTC"
    assert len(m["content_hash"]) == 16


def test_inventory_on_empty_store(tmp_path):
    inv = S.inventory(root=tmp_path)
    assert inv.height == 0
    assert "content_hash" in inv.columns


def test_inventory_reports_range(tmp_path):
    S.write(_frame([_bar(1), _bar(4)]), root=tmp_path, source="test")
    inv = S.inventory(root=tmp_path)
    assert inv.height == 1
    row = inv.row(0, named=True)
    assert row["asset"] == "BTC"
    assert row["rows"] == 2


def test_multiple_assets_stay_separate(tmp_path):
    S.write(
        _frame([_bar(1, asset="BTC"), _bar(1, asset="ETH")]), root=tmp_path, source="test"
    )
    df = S.read(root=tmp_path, interval_s=DAY)
    assert sorted(df["asset"].unique().to_list()) == ["BTC", "ETH"]
    only_eth = S.read(root=tmp_path, assets=["ETH"], interval_s=DAY)
    assert only_eth["asset"].unique().to_list() == ["ETH"]
