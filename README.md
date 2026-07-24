# RangeBar

A high-performance, thread-safe Rust + PyO3 Python extension for generating **Range Bars** from tick, bulk price, or OHLC data. Built with [Polars](https://pola.rs/) for fast DataFrame operations and [maturin](https://github.com/PyO3/maturin) for building.

## What Are Range Bars?

Range bars are a type of price chart where each bar represents a fixed price range rather than a fixed time period. A new bar is created only when price moves beyond the current bar's upper or lower boundary, making them useful for filtering out noise in volatile markets.

### Core Rules

- Each bar's high-low range equals exactly the specified `range_size`.
- Each bar opens outside the previous bar's high-low range.
- Each bar closes at either its high or its low.
- Large price jumps generate multiple completed bars (one per boundary crossed).
- An unfinished (in-progress) bar is always included in output.

## Requirements

| Dependency | Version |
|---|---|
| Python | >= 3.9 |
| Rust | Latest stable (1.83+) |
| Polars (Python) | >= 1.0 |
| Polars (Rust) | 0.54 |
| PyO3 | 0.28 |
| pyo3-polars | 0.27 |
| maturin | >= 1.0 |

## Installation

### From Source

```bash
pip install maturin polars pandas
maturin build
pip install target/wheels/rangebar-*.whl
```

Or for development (requires a virtual environment):

```bash
python -m venv .venv
source .venv/bin/activate  # Linux/macOS
# .venv\Scripts\activate   # Windows
pip install maturin polars pandas
maturin develop
```

## Quick Start

```python
import polars as pl
from datetime import datetime
from rangebar import RangeBar

rb = RangeBar(range_size=10)

rb.update_tick(ltp=100.0, ltq=5.0, datetime=datetime(2024, 1, 1, 12, 0, 0))
rb.update_tick(ltp=105.0, ltq=3.0, datetime=datetime(2024, 1, 1, 12, 1, 0))
rb.update_tick(ltp=112.0, ltq=2.0, datetime=datetime(2024, 1, 1, 12, 2, 0))

df = rb.get()
print(df)
```

Output:

```
shape: (2, 9)
┌───────┬───────┬───────┬───────┬───┬────────────┬───────────┬────────────────┬──────────────┐
│ open  ┆ high  ┆ low   ┆ close ┆ … ┆ tick_count ┆ direction ┆ datetime_start ┆ datetime_end │
│ ---   ┆ ---   ┆ ---   ┆ ---   ┆   ┆ ---        ┆ ---       ┆ ---            ┆ ---          │
│ f64   ┆ f64   ┆ f64   ┆ f64   ┆   ┆ u64        ┆ i8        ┆ datetime[ns]   ┆ datetime[ns] │
╞═══════╪═══════╪═══════╪═══════╪═══╪════════════╪═══════════╪════════════════╪══════════════╡
│ 100.0 ┆ 110.0 ┆ 100.0 ┆ 110.0 ┆ … ┆ 2          ┆ 1         ┆ 2024-01-01     ┆ 2024-01-01   │
│       ┆       ┆       ┆       ┆   ┆            ┆           ┆ 12:00:00       ┆ 12:02:00     │
│ 110.0 ┆ 112.0 ┆ 110.0 ┆ 112.0 ┆ … ┆ 1          ┆ 1         ┆ 2024-01-01     ┆ 2024-01-01   │
│       ┆       ┆       ┆       ┆   ┆            ┆           ┆ 12:02:00       ┆ 12:02:00     │
└───────┴───────┴───────┴───────┴───┴────────────┴───────────┴────────────────┴──────────────┘
```

## API Reference

### `RangeBar(range_size: float)`

Create a new RangeBar instance.

| Parameter | Type | Description |
|---|---|---|
| `range_size` | `float` | The price range for each bar. Must be > 0. Can be fractional (e.g. `0.5`). Rounded to 4 decimal places. |

```python
rb = RangeBar(range_size=10)
rb = RangeBar(range_size=0.25)
```

Raises `ValueError` if `range_size <= 0`.

---

### `update_tick(ltp, ltq, datetime, timestamp=None)`

Add a single tick to the range bar computation.

| Parameter | Type | Required | Description |
|---|---|---|---|
| `ltp` | `float` | Yes | Last traded price. Must be > 0 (prices <= 0 are silently ignored). |
| `ltq` | `float` | Yes | Last traded quantity (volume). Must be >= 0. |
| `datetime` | `datetime` | Yes | Tick timestamp. Accepts `datetime.datetime`, `pandas.Timestamp`, or Polars datetime. |
| `timestamp` | `int` | No | Optional Unix timestamp (integer). |

```python
from datetime import datetime, timezone

rb.update_tick(ltp=100.50, ltq=10.0, datetime=datetime(2024, 1, 1, 12, 0, 0))
rb.update_tick(ltp=100.50, ltq=5.0, datetime=datetime(2024, 1, 1, 12, 0, 1), timestamp=1704110401)
```

Raises `ValueError` if `ltq < 0`.

---

### `update_price(df)`

Add bulk price data from a Polars or Pandas DataFrame.

| Parameter | Type | Required | Description |
|---|---|---|---|
| `df` | `pl.DataFrame` or `pd.DataFrame` | Yes | DataFrame with required columns (see below). |

**Required columns:**

| Column | Type | Description |
|---|---|---|
| `price` | numeric | Price value. Must be > 0. |
| `volume` | numeric | Volume. Must be >= 0. |
| `datetime` | datetime | Tick timestamp. Must not be null. |

**Optional columns:**

| Column | Type | Description |
|---|---|---|
| `timestamp` | integer | Unix timestamp. Must not be null if present. |

```python
df = pl.DataFrame({
    "price": [100.0, 105.0, 110.0],
    "volume": [10.0, 20.0, 15.0],
    "datetime": [
        datetime(2024, 1, 1, 12, 0, 0),
        datetime(2024, 1, 1, 12, 1, 0),
        datetime(2024, 1, 1, 12, 2, 0),
    ],
})
rb.update_price(df)
```

Raises `ValueError` if any validation fails (nulls, negative volume, non-positive prices, etc.).

---

### `update_ohlc(df)`

Add OHLC data from a Polars or Pandas DataFrame. Each OHLC row is expanded into 4 synthetic ticks internally.

| Parameter | Type | Required | Description |
|---|---|---|---|
| `df` | `pl.DataFrame` or `pd.DataFrame` | Yes | DataFrame with required columns (see below). |

**Required columns:**

| Column | Type | Description |
|---|---|---|
| `open` | numeric | Open price. Must be > 0. |
| `high` | numeric | High price. Must be > 0 and >= max(open, close, low). |
| `low` | numeric | Low price. Must be > 0 and <= min(open, close, high). |
| `close` | numeric | Close price. Must be > 0. |
| `volume` | numeric | Volume. Must be >= 0. |
| `datetime` | datetime | Bar timestamp. Must not be null. |

**Optional columns:**

| Column | Type | Description |
|---|---|---|
| `timestamp` | integer | Unix timestamp. Must not be null if present. |

**Intrabar tick order:**
- Bullish bar (`close >= open`): Open, Low, High, Close
- Bearish bar (`close < open`): Open, High, Low, Close

**Volume allocation:** Row volume is divided equally among all range bars touched by that row's synthetic ticks. The last bar receives any floating-point remainder. Integer-typed volumes are rounded during allocation.

```python
df = pl.DataFrame({
    "open": [100.0, 110.0],
    "high": [115.0, 125.0],
    "low": [95.0, 108.0],
    "close": [110.0, 120.0],
    "volume": [100.0, 80.0],
    "datetime": [
        datetime(2024, 1, 1, 12, 0, 0),
        datetime(2024, 1, 1, 13, 0, 0),
    ],
})
rb.update_ohlc(df)
```

Raises `ValueError` if OHLC constraints are violated (e.g. `high < max(open, close, low)`).

---

### `get() -> pl.DataFrame`

Returns a **new** Polars DataFrame containing all range bars from oldest to newest. Each call constructs a fresh DataFrame independent of internal storage.

**Output columns:**

| Column | Type | Description |
|---|---|---|
| `open` | `Float64` | Bar open price (rounded to 4 decimal places). |
| `high` | `Float64` | Bar high price (rounded to 4 decimal places). |
| `low` | `Float64` | Bar low price (rounded to 4 decimal places). |
| `close` | `Float64` | Bar close price (rounded to 4 decimal places). |
| `volume` | `Float64` | Accumulated volume for the bar. |
| `tick_count` | `UInt64` | Number of processed ticks in this bar (excludes synthetic boundary ticks). |
| `direction` | `Int8` | `1` if close > open, `-1` if close < open, `0` if equal. |
| `datetime_start` | `Datetime(ns)` | Timestamp of the first tick in this bar. |
| `datetime_end` | `Datetime(ns)` | Timestamp of the last tick in this bar. |
| `timestamp_start` | `Int64` | *(Only if all input events have timestamps)* Unix timestamp of first tick. |
| `timestamp_end` | `Int64` | *(Only if all input events have timestamps)* Unix timestamp of last tick. |

```python
df = rb.get()
print(df.schema)
```

---

### `reset()`

Clears all stored data while preserving `range_size`.

```python
rb.reset()
df = rb.get()
assert df.shape == (0, 9)
```

## Usage Examples

### Large Price Jump (Upward)

```python
rb = RangeBar(range_size=10)
rb.update_tick(ltp=100.0, ltq=1.0, datetime=datetime(2024, 1, 1, 12, 0, 0))
rb.update_tick(ltp=145.0, ltq=1.0, datetime=datetime(2024, 1, 1, 12, 1, 0))

df = rb.get()
# Produces 5 bars: 100-110, 110-120, 120-130, 130-140, 140-145
```

### Large Price Jump (Downward)

```python
rb = RangeBar(range_size=10)
rb.update_tick(ltp=100.0, ltq=1.0, datetime=datetime(2024, 1, 1, 12, 0, 0))
rb.update_tick(ltp=65.0, ltq=1.0, datetime=datetime(2024, 1, 1, 12, 1, 0))

df = rb.get()
# Produces 4 bars: 100-90, 90-80, 80-70, 70-65
```

### Mixing Input Methods

All three input methods can be used in any order on the same instance:

```python
rb = RangeBar(range_size=10)

rb.update_tick(ltp=100.0, ltq=1.0, datetime=datetime(2024, 1, 1, 12, 0, 0))

price_df = pl.DataFrame({
    "price": [105.0, 112.0],
    "volume": [2.0, 3.0],
    "datetime": [datetime(2024, 1, 1, 12, 1, 0), datetime(2024, 1, 1, 12, 2, 0)],
})
rb.update_price(price_df)

ohlc_df = pl.DataFrame({
    "open": [112.0], "high": [125.0], "low": [108.0], "close": [120.0],
    "volume": [50.0], "datetime": [datetime(2024, 1, 1, 13, 0, 0)],
})
rb.update_ohlc(ohlc_df)

df = rb.get()
```

### Pandas Input

Pandas DataFrames are automatically converted to Polars internally:

```python
import pandas as pd

pdf = pd.DataFrame({
    "price": [100.0, 105.0, 110.0],
    "volume": [10.0, 20.0, 15.0],
    "datetime": pd.to_datetime(["2024-01-01 12:00", "2024-01-01 12:01", "2024-01-01 12:02"]),
})
rb.update_price(pdf)
```

### Timezone-Aware Datetimes

Both timezone-aware and timezone-naive datetimes are accepted. Everything is converted to UTC internally:

```python
from datetime import timezone, timedelta

est = timezone(timedelta(hours=-5))
rb.update_tick(ltp=100.0, ltq=1.0, datetime=datetime(2024, 1, 1, 7, 0, 0, tzinfo=est))
rb.update_tick(ltp=105.0, ltq=1.0, datetime=datetime(2024, 1, 1, 12, 1, 0))  # naive = UTC
```

### Historical Data (Out-of-Order Insertion)

Data arriving out of chronological order is handled correctly via stable sorting:

```python
rb = RangeBar(range_size=10)
rb.update_tick(ltp=110.0, ltq=1.0, datetime=datetime(2024, 1, 1, 12, 2, 0))
rb.update_tick(ltp=100.0, ltq=1.0, datetime=datetime(2024, 1, 1, 12, 0, 0))  # older data arrives later

df = rb.get()  # Bars are computed correctly from sorted events
```

### With Timestamps

When all input events include timestamps, `timestamp_start` and `timestamp_end` columns appear in output:

```python
df = pl.DataFrame({
    "price": [100.0, 110.0],
    "volume": [1.0, 2.0],
    "datetime": [datetime(2024, 1, 1, 12, 0, 0), datetime(2024, 1, 1, 12, 1, 0)],
    "timestamp": [1704110400, 1704110460],
})
rb.update_price(df)

result = rb.get()
print(result.columns)
# ['open', 'high', 'low', 'close', 'volume', 'tick_count', 'direction',
#  'datetime_start', 'datetime_end', 'timestamp_start', 'timestamp_end']
```

## Design Decisions

| Aspect | Approach |
|---|---|
| **Thread safety** | `Arc<RwLock<...>>` from `std` |
| **GIL release** | `Python::detach()` during heavy computation |
| **Datetime precision** | Nanoseconds internally |
| **Timezone handling** | All datetimes converted to UTC; naive treated as UTC |
| **Sort stability** | By `(datetime, insertion_sequence_number)` |
| **Price rounding** | 4 decimal places via `round()` before any comparison |
| **Comparisons** | Exact (no epsilon) after rounding |
| **Output isolation** | `get()` always constructs a new DataFrame |
| **Determinism** | Internal storage is deterministic for identical inputs |
| **Recomputation** | Full recompute from stored events when historical data arrives |

## Project Structure

```
rangebar/
├── Cargo.toml        # Rust dependencies and build config
├── pyproject.toml    # Python package metadata and maturin config
├── lib.rs            # Python module entry point (exposes RangeBar class)
└── rangebar.rs       # All implementation logic
```

## Building

```bash
# Debug build
maturin build

# Release build
maturin build --release

# Install into active virtualenv
maturin develop

# Lint
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
```

## License

MIT
