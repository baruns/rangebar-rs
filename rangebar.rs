use std::sync::{Arc, RwLock};

use polars::prelude::*;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3_polars::PyDataFrame;

const MAX_RANGE_BARS: usize = 10_000_000;

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

fn datetime_to_ns(py: Python<'_>, dt: &Bound<'_, PyAny>) -> PyResult<i64> {
    if let Ok(value) = dt.getattr("value") {
        if let Ok(ns) = value.extract::<i64>() {
            return Ok(ns);
        }
    }

    let tzinfo = dt
        .getattr("tzinfo")
        .map_err(|_| PyValueError::new_err("datetime parameter must be a datetime object"))?;

    if !tzinfo.is_none() {
        let datetime_mod = py.import("datetime")?;
        let utc = datetime_mod.getattr("timezone")?.getattr("utc")?;
        let dt_utc = dt.call_method1("astimezone", (&utc,))?;
        let calendar = py.import("calendar")?;
        let timetuple = dt_utc.call_method0("timetuple")?;
        let ts = calendar.call_method1("timegm", (timetuple,))?;
        let ts_i64: i64 = ts.extract()?;
        let microsecond = dt_utc.getattr("microsecond")?;
        let us: i64 = microsecond.extract()?;
        return Ok(ts_i64 * 1_000_000_000 + us * 1_000);
    }

    let calendar = py.import("calendar")?;
    let timetuple = dt.call_method0("timetuple")?;
    let ts = calendar.call_method1("timegm", (timetuple,))?;
    let ts_i64: i64 = ts.extract()?;
    let microsecond = dt.getattr("microsecond")?;
    let us: i64 = microsecond.extract()?;
    Ok(ts_i64 * 1_000_000_000 + us * 1_000)
}

fn to_polars_df(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<DataFrame> {
    if let Ok(py_df) = obj.extract::<PyDataFrame>() {
        return Ok(py_df.into());
    }
    let pl = py
        .import("polars")
        .map_err(|_| PyValueError::new_err("polars Python package is required"))?;
    let converted = pl.call_method1("from_pandas", (obj,)).map_err(|e| {
        PyValueError::new_err(format!("input must be a Polars or Pandas DataFrame: {}", e))
    })?;
    let py_df: PyDataFrame = converted.extract().map_err(|e| {
        PyValueError::new_err(format!("failed to convert to Polars DataFrame: {}", e))
    })?;
    Ok(py_df.into())
}

#[derive(Clone, Debug)]
enum EventKind {
    Ohlc {
        prices: [f64; 4],
        volume: f64,
        is_int_volume: bool,
    },
    Price {
        volume: f64,
    },
    Single {
        volume: f64,
    },
}

#[derive(Clone, Debug)]
struct RawEvent {
    price: Option<f64>,
    datetime_ns: i64,
    timestamp: Option<i64>,
    seq: u64,
    kind: EventKind,
}

struct InnerState {
    events: Vec<RawEvent>,
    next_seq: u64,
}

#[derive(Clone, Debug)]
struct BarBuilder {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    tick_count: u64,
    datetime_start: i64,
    datetime_end: i64,
    timestamp_start: Option<i64>,
    timestamp_end: Option<i64>,
}

#[derive(Debug)]
struct OutputBar {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    tick_count: u64,
    datetime_start: i64,
    datetime_end: i64,
    timestamp_start: Option<i64>,
    timestamp_end: Option<i64>,
}

fn push_bar(bars: &mut Vec<BarBuilder>, bar: BarBuilder) -> Result<(), String> {
    if bars.len() >= MAX_RANGE_BARS {
        return Err(format!(
            "range-bar output exceeds the safety limit of {MAX_RANGE_BARS} bars; increase range_size or check the input price scale"
        ));
    }
    bars.try_reserve(1).map_err(|_| {
        "not enough memory to create range bars; increase range_size or process a smaller price span"
            .to_string()
    })?;
    bars.push(bar);
    Ok(())
}

fn process_price(
    all_bars: &mut Vec<BarBuilder>,
    price: f64,
    event: &RawEvent,
    volume: Option<f64>,
    range_size: f64,
) -> Result<usize, String> {
    if all_bars.is_empty() {
        push_bar(
            all_bars,
            BarBuilder {
                open: price,
                high: price,
                low: price,
                close: price,
                volume: volume.unwrap_or(0.0),
                tick_count: 1,
                datetime_start: event.datetime_ns,
                datetime_end: event.datetime_ns,
                timestamp_start: event.timestamp,
                timestamp_end: event.timestamp,
            },
        )?;
        return Ok(0);
    }

    let current = all_bars.last().expect("checked non-empty");
    let upper = round4(current.open + range_size);
    let lower = round4(current.open - range_size);
    let required_new_bars = if price > upper {
        let first_open = round4(current.low + range_size);
        let remaining = (price - round4(first_open + range_size)).max(0.0);
        1.0 + (remaining / range_size).ceil()
    } else if price < lower {
        let first_open = round4(current.high - range_size);
        let remaining = (round4(first_open - range_size) - price).max(0.0);
        1.0 + (remaining / range_size).ceil()
    } else {
        0.0
    };
    if !required_new_bars.is_finite()
        || required_new_bars > (MAX_RANGE_BARS - all_bars.len()) as f64
    {
        return Err(format!(
            "this price movement would exceed the safety limit of {MAX_RANGE_BARS} range bars; increase range_size or check the input price scale"
        ));
    }
    all_bars
        .try_reserve(required_new_bars as usize)
        .map_err(|_| {
            "not enough memory to create range bars; increase range_size or process a smaller price span"
                .to_string()
        })?;

    loop {
        let bar = all_bars.last_mut().expect("checked non-empty");
        let upper = round4(bar.open + range_size);
        let lower = round4(bar.open - range_size);

        if price > upper {
            bar.high = bar.low + range_size;
            bar.close = bar.high;
            bar.datetime_end = event.datetime_ns;
            bar.timestamp_end = event.timestamp;
            let new_open = bar.close;
            push_bar(
                all_bars,
                BarBuilder {
                    open: new_open,
                    high: new_open,
                    low: new_open,
                    close: new_open,
                    volume: 0.0,
                    tick_count: 0,
                    datetime_start: event.datetime_ns,
                    datetime_end: event.datetime_ns,
                    timestamp_start: event.timestamp,
                    timestamp_end: event.timestamp,
                },
            )?;
        } else if price < lower {
            bar.low = bar.high - range_size;
            bar.close = bar.low;
            bar.datetime_end = event.datetime_ns;
            bar.timestamp_end = event.timestamp;
            let new_open = bar.close;
            push_bar(
                all_bars,
                BarBuilder {
                    open: new_open,
                    high: new_open,
                    low: new_open,
                    close: new_open,
                    volume: 0.0,
                    tick_count: 0,
                    datetime_start: event.datetime_ns,
                    datetime_end: event.datetime_ns,
                    timestamp_start: event.timestamp,
                    timestamp_end: event.timestamp,
                },
            )?;
        } else {
            let bar = all_bars.last_mut().expect("checked non-empty");
            let updated_high = bar.high.max(price);
            let updated_low = bar.low.min(price);

            if updated_high - updated_low > range_size {
                bar.high = updated_high;
                bar.low = updated_low;
                bar.datetime_end = event.datetime_ns;
                bar.timestamp_end = event.timestamp;
                if price > bar.open {
                    bar.high = bar.low + range_size;
                    bar.close = bar.high;
                } else {
                    bar.low = bar.high - range_size;
                    bar.close = bar.low;
                }
                let new_open = bar.close;
                push_bar(
                    all_bars,
                    BarBuilder {
                        open: new_open,
                        high: new_open,
                        low: new_open,
                        close: new_open,
                        volume: 0.0,
                        tick_count: 0,
                        datetime_start: event.datetime_ns,
                        datetime_end: event.datetime_ns,
                        timestamp_start: event.timestamp,
                        timestamp_end: event.timestamp,
                    },
                )?;
            } else {
                bar.high = updated_high;
                bar.low = updated_low;
                bar.close = price;
                bar.tick_count += 1;
                bar.datetime_end = event.datetime_ns;
                bar.timestamp_end = event.timestamp;
                if let Some(volume) = volume {
                    bar.volume += volume;
                }
                return Ok(all_bars.len() - 1);
            }
        }
    }
}

fn compute_range_bars(
    events: &[RawEvent],
    range_size: f64,
) -> Result<(Vec<OutputBar>, bool), String> {
    let has_all_timestamps = !events.is_empty() && events.iter().all(|e| e.timestamp.is_some());

    if events.is_empty() {
        return Ok((Vec::new(), has_all_timestamps));
    }

    let mut all_bars: Vec<BarBuilder> = Vec::new();

    for event in events {
        match &event.kind {
            EventKind::Price { volume } | EventKind::Single { volume } => {
                process_price(
                    &mut all_bars,
                    event.price.expect("price event"),
                    event,
                    Some(*volume),
                    range_size,
                )?;
            }
            EventKind::Ohlc {
                prices,
                volume,
                is_int_volume,
            } => {
                let first_bar_idx = all_bars.len().saturating_sub(1);
                for price in prices {
                    process_price(&mut all_bars, *price, event, None, range_size)?;
                }

                let last_bar_idx = all_bars.len() - 1;
                let count = last_bar_idx - first_bar_idx + 1;
                let portion = if *is_int_volume {
                    (*volume / count as f64).round()
                } else {
                    *volume / count as f64
                };
                let mut allocated = 0.0;
                for (position, bar_idx) in (first_bar_idx..=last_bar_idx).enumerate() {
                    let amount = if position + 1 == count {
                        *volume - allocated
                    } else {
                        allocated += portion;
                        portion
                    };
                    all_bars[bar_idx].volume += amount;
                }
            }
        }
    }

    let output: Vec<OutputBar> = all_bars
        .into_iter()
        .map(|b| OutputBar {
            open: round4(b.open),
            high: round4(b.high),
            low: round4(b.low),
            close: round4(b.close),
            volume: b.volume,
            tick_count: b.tick_count,
            datetime_start: b.datetime_start,
            datetime_end: b.datetime_end,
            timestamp_start: b.timestamp_start,
            timestamp_end: b.timestamp_end,
        })
        .collect();

    Ok((output, has_all_timestamps))
}

fn build_dataframe(bars: &[OutputBar], has_all_timestamps: bool) -> Result<DataFrame, String> {
    let n = bars.len();

    let mut opens: Vec<f64> = Vec::with_capacity(n);
    let mut highs: Vec<f64> = Vec::with_capacity(n);
    let mut lows: Vec<f64> = Vec::with_capacity(n);
    let mut closes: Vec<f64> = Vec::with_capacity(n);
    let mut volumes: Vec<f64> = Vec::with_capacity(n);
    let mut tick_counts: Vec<u64> = Vec::with_capacity(n);
    let mut directions: Vec<i8> = Vec::with_capacity(n);
    let mut dt_starts: Vec<i64> = Vec::with_capacity(n);
    let mut dt_ends: Vec<i64> = Vec::with_capacity(n);
    let mut ts_starts: Vec<i64> = Vec::with_capacity(n);
    let mut ts_ends: Vec<i64> = Vec::with_capacity(n);

    for bar in bars {
        let o = round4(bar.open);
        let h = round4(bar.high);
        let l = round4(bar.low);
        let c = round4(bar.close);

        opens.push(o);
        highs.push(h);
        lows.push(l);
        closes.push(c);
        volumes.push(bar.volume);
        tick_counts.push(bar.tick_count);

        let dir: i8 = if c > o {
            1
        } else if c < o {
            -1
        } else {
            0
        };
        directions.push(dir);

        dt_starts.push(bar.datetime_start);
        dt_ends.push(bar.datetime_end);

        if has_all_timestamps {
            ts_starts.push(bar.timestamp_start.unwrap_or(0));
            ts_ends.push(bar.timestamp_end.unwrap_or(0));
        }
    }

    let dt_start_series = Series::new("datetime_start".into(), dt_starts)
        .cast(&DataType::Datetime(TimeUnit::Nanoseconds, None))
        .map_err(|e| e.to_string())?;

    let dt_end_series = Series::new("datetime_end".into(), dt_ends)
        .cast(&DataType::Datetime(TimeUnit::Nanoseconds, None))
        .map_err(|e| e.to_string())?;

    let dir_series = Int8Chunked::from_vec("direction".into(), directions).into_series();

    let mut columns: Vec<Column> = vec![
        Series::new("open".into(), opens).into_column(),
        Series::new("high".into(), highs).into_column(),
        Series::new("low".into(), lows).into_column(),
        Series::new("close".into(), closes).into_column(),
        Series::new("volume".into(), volumes).into_column(),
        Series::new("tick_count".into(), tick_counts).into_column(),
        dir_series.into_column(),
        dt_start_series.into_column(),
        dt_end_series.into_column(),
    ];

    if has_all_timestamps {
        columns.push(Series::new("timestamp_start".into(), ts_starts).into_column());
        columns.push(Series::new("timestamp_end".into(), ts_ends).into_column());
    }

    DataFrame::new_infer_height(columns).map_err(|e| e.to_string())
}

fn get_series(df: &DataFrame, name: &str) -> PyResult<Series> {
    let col = df
        .column(name)
        .map_err(|_| PyValueError::new_err(format!("missing column: {}", name)))?;
    Ok(col.as_materialized_series().clone())
}

fn extract_datetime_ns(col: &Series) -> PyResult<Vec<i64>> {
    match col.dtype() {
        DataType::Datetime(_, Some(_)) => {
            let utc_col = col
                .cast(&DataType::Datetime(
                    TimeUnit::Nanoseconds,
                    Some(TimeZone::UTC),
                ))
                .map_err(|e| PyValueError::new_err(format!("datetime conversion error: {}", e)))?;
            let naive_col = utc_col
                .cast(&DataType::Datetime(TimeUnit::Nanoseconds, None))
                .map_err(|e| PyValueError::new_err(format!("datetime conversion error: {}", e)))?;
            let ns_col = naive_col
                .cast(&DataType::Int64)
                .map_err(|e| PyValueError::new_err(format!("datetime conversion error: {}", e)))?;
            let ca = ns_col
                .i64()
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            if ca.null_count() > 0 {
                return Err(PyValueError::new_err(
                    "datetime column contains null values",
                ));
            }
            Ok(ca.into_no_null_iter().collect())
        }
        DataType::Datetime(_, None) => {
            let ns_col = col
                .cast(&DataType::Datetime(TimeUnit::Nanoseconds, None))
                .map_err(|e| PyValueError::new_err(format!("datetime conversion error: {}", e)))?;
            let int_col = ns_col
                .cast(&DataType::Int64)
                .map_err(|e| PyValueError::new_err(format!("datetime conversion error: {}", e)))?;
            let ca = int_col
                .i64()
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            if ca.null_count() > 0 {
                return Err(PyValueError::new_err(
                    "datetime column contains null values",
                ));
            }
            Ok(ca.into_no_null_iter().collect())
        }
        _ => Err(PyValueError::new_err(
            "datetime column must be Datetime type",
        )),
    }
}

fn extract_timestamps(col: &Series) -> PyResult<Vec<i64>> {
    let is_int = matches!(
        col.dtype(),
        DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
    );
    if !is_int {
        return Err(PyValueError::new_err(
            "timestamp column must be integer type",
        ));
    }
    let i64_col = col
        .cast(&DataType::Int64)
        .map_err(|e| PyValueError::new_err(format!("timestamp conversion error: {}", e)))?;
    let ca = i64_col
        .i64()
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    if ca.null_count() > 0 {
        return Err(PyValueError::new_err(
            "timestamp column contains null values",
        ));
    }
    Ok(ca.into_no_null_iter().collect())
}

fn extract_price_col(col: &Series, col_name: &str) -> PyResult<Vec<f64>> {
    let f64_col = col
        .cast(&DataType::Float64)
        .map_err(|e| PyValueError::new_err(format!("{} conversion error: {}", col_name, e)))?;
    let ca = f64_col
        .f64()
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    if ca.null_count() > 0 {
        return Err(PyValueError::new_err(format!(
            "{} column contains null values",
            col_name
        )));
    }
    let values: Vec<f64> = ca.into_no_null_iter().collect();
    for v in &values {
        if !v.is_finite() {
            return Err(PyValueError::new_err(format!(
                "{} contains non-finite values",
                col_name
            )));
        }
    }
    Ok(values.into_iter().map(round4).collect())
}

fn extract_volume_col(col: &Series) -> PyResult<(Vec<f64>, bool)> {
    let is_int = matches!(
        col.dtype(),
        DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
    );
    let f64_col = col
        .cast(&DataType::Float64)
        .map_err(|e| PyValueError::new_err(format!("volume conversion error: {}", e)))?;
    let ca = f64_col
        .f64()
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    if ca.null_count() > 0 {
        return Err(PyValueError::new_err("volume column contains null values"));
    }
    let values: Vec<f64> = ca.into_no_null_iter().collect();
    for v in &values {
        if !v.is_finite() {
            return Err(PyValueError::new_err("volume contains non-finite values"));
        }
        if *v < 0.0 {
            return Err(PyValueError::new_err("volume must be >= 0"));
        }
    }
    Ok((values, is_int))
}

fn append_events_and_sort(state: &mut InnerState, new_events: Vec<RawEvent>) -> Result<(), String> {
    let needs_sort = new_events
        .windows(2)
        .any(|pair| pair[1].datetime_ns < pair[0].datetime_ns)
        || state.events.last().is_some_and(|last| {
            new_events
                .first()
                .is_some_and(|first| first.datetime_ns < last.datetime_ns)
        });
    state.events.try_reserve(new_events.len()).map_err(|_| {
        "not enough memory to retain input data; process smaller batches and call reset between datasets"
            .to_string()
    })?;
    state.events.extend(new_events);
    if needs_sort {
        state
            .events
            .sort_by(|a, b| a.datetime_ns.cmp(&b.datetime_ns).then(a.seq.cmp(&b.seq)));
    }
    Ok(())
}

#[pyclass]
pub struct RangeBar {
    range_size: f64,
    state: Arc<RwLock<InnerState>>,
}

#[pymethods]
impl RangeBar {
    #[new]
    #[pyo3(signature = (range_size))]
    fn new(range_size: f64) -> PyResult<Self> {
        if !range_size.is_finite() || range_size <= 0.0 {
            return Err(PyValueError::new_err("range_size must be > 0"));
        }
        let rounded = round4(range_size);
        if rounded <= 0.0 {
            return Err(PyValueError::new_err(
                "range_size must be > 0 after rounding to 4 decimal places",
            ));
        }
        Ok(RangeBar {
            range_size: rounded,
            state: Arc::new(RwLock::new(InnerState {
                events: Vec::new(),
                next_seq: 0,
            })),
        })
    }

    #[pyo3(signature = (ltp, ltq, datetime, timestamp=None))]
    fn update_tick(
        &self,
        py: Python<'_>,
        ltp: f64,
        ltq: f64,
        datetime: &Bound<'_, PyAny>,
        timestamp: Option<i64>,
    ) -> PyResult<()> {
        if !ltq.is_finite() || ltq < 0.0 {
            return Err(PyValueError::new_err("volume must be >= 0"));
        }
        if !ltp.is_finite() {
            return Ok(());
        }
        let price = round4(ltp);
        if price <= 0.0 {
            return Ok(());
        }
        let datetime_ns = datetime_to_ns(py, datetime)?;

        let mut state = self
            .state
            .write()
            .map_err(|_| PyValueError::new_err("lock poisoned"))?;

        let seq = state.next_seq;
        state.next_seq += 1;

        let event = RawEvent {
            price: Some(price),
            datetime_ns,
            timestamp,
            seq,
            kind: EventKind::Single { volume: ltq },
        };

        append_events_and_sort(&mut state, vec![event]).map_err(PyValueError::new_err)?;
        Ok(())
    }

    fn update_price(&self, py: Python<'_>, df: &Bound<'_, PyAny>) -> PyResult<()> {
        let polars_df = to_polars_df(py, df)?;

        if polars_df.height() == 0 {
            return Ok(());
        }

        let price_series = get_series(&polars_df, "price")?;
        let volume_series = get_series(&polars_df, "volume")?;
        let datetime_series = get_series(&polars_df, "datetime")?;
        let timestamp_series = polars_df
            .column("timestamp")
            .ok()
            .map(|c| c.as_materialized_series().clone());

        let prices = extract_price_col(&price_series, "price")?;
        let (volumes, _) = extract_volume_col(&volume_series)?;
        let datetimes = extract_datetime_ns(&datetime_series)?;
        let timestamps = if let Some(ref ts_col) = timestamp_series {
            Some(extract_timestamps(ts_col)?)
        } else {
            None
        };

        let n = prices.len();
        let state_arc = self.state.clone();

        let result: Result<Vec<RawEvent>, String> = py.detach(move || {
            let mut state = state_arc.write().map_err(|_| "lock poisoned".to_string())?;
            let mut events = Vec::new();
            events.try_reserve(n).map_err(|_| {
                "not enough memory to retain input data; process a smaller batch".to_string()
            })?;
            for i in 0..n {
                // Skip rows where price <= 0
                if prices[i] <= 0.0 {
                    continue;
                }
                let ts = timestamps.as_ref().map(|t| t[i]);
                events.push(RawEvent {
                    price: Some(prices[i]),
                    datetime_ns: datetimes[i],
                    timestamp: ts,
                    seq: state.next_seq,
                    kind: EventKind::Price { volume: volumes[i] },
                });
                state.next_seq += 1;
            }
            Ok(events)
        });

        let new_events = result.map_err(PyValueError::new_err)?;

        let mut state = self
            .state
            .write()
            .map_err(|_| PyValueError::new_err("lock poisoned"))?;
        append_events_and_sort(&mut state, new_events).map_err(PyValueError::new_err)?;
        Ok(())
    }

    fn update_ohlc(&self, py: Python<'_>, df: &Bound<'_, PyAny>) -> PyResult<()> {
        let polars_df = to_polars_df(py, df)?;

        if polars_df.height() == 0 {
            return Ok(());
        }

        let open_series = get_series(&polars_df, "open")?;
        let high_series = get_series(&polars_df, "high")?;
        let low_series = get_series(&polars_df, "low")?;
        let close_series = get_series(&polars_df, "close")?;
        let volume_series = get_series(&polars_df, "volume")?;
        let datetime_series = get_series(&polars_df, "datetime")?;
        let timestamp_series = polars_df
            .column("timestamp")
            .ok()
            .map(|c| c.as_materialized_series().clone());

        let opens = extract_price_col(&open_series, "open")?;
        let mut highs = extract_price_col(&high_series, "high")?;
        let mut lows = extract_price_col(&low_series, "low")?;
        let closes = extract_price_col(&close_series, "close")?;
        let (volumes, is_int_volume) = extract_volume_col(&volume_series)?;
        let datetimes = extract_datetime_ns(&datetime_series)?;
        let timestamps = if let Some(ref ts_col) = timestamp_series {
            Some(extract_timestamps(ts_col)?)
        } else {
            None
        };

        let n = opens.len();
        for i in 0..n {
            let o = opens[i];
            let c = closes[i];
            let l = lows[i];
            let h = highs[i];

            if h < o.max(c).max(l) {
                highs[i] = o.max(c).max(l);
            }
            if l > o.min(c).min(highs[i]) {
                lows[i] = o.min(c).min(highs[i]);
            }
        }

        let state_arc = self.state.clone();
        let result: Result<Vec<RawEvent>, String> = py.detach(move || {
            let mut state = state_arc.write().map_err(|_| "lock poisoned".to_string())?;
            let mut events = Vec::new();
            events.try_reserve(n).map_err(|_| {
                "not enough memory to retain OHLC data; process a smaller batch".to_string()
            })?;
            for i in 0..n {
                let o = opens[i];
                let h = highs[i];
                let l = lows[i];
                let c = closes[i];

                // Skip rows where any of open/high/low/close <= 0
                if o <= 0.0 || h <= 0.0 || l <= 0.0 || c <= 0.0 {
                    continue;
                }

                let dt = datetimes[i];
                let ts = timestamps.as_ref().map(|t| t[i]);
                let tick_prices = if c >= o { [o, l, h, c] } else { [o, h, l, c] };
                events.push(RawEvent {
                    price: None,
                    datetime_ns: dt,
                    timestamp: ts,
                    seq: state.next_seq,
                    kind: EventKind::Ohlc {
                        prices: tick_prices,
                        volume: volumes[i],
                        is_int_volume,
                    },
                });
                state.next_seq += 1;
            }
            Ok(events)
        });

        let new_events = result.map_err(PyValueError::new_err)?;

        let mut state = self
            .state
            .write()
            .map_err(|_| PyValueError::new_err("lock poisoned"))?;
        append_events_and_sort(&mut state, new_events).map_err(PyValueError::new_err)?;
        Ok(())
    }

    fn get(&self, py: Python<'_>) -> PyResult<PyDataFrame> {
        let events = {
            let state = self
                .state
                .read()
                .map_err(|_| PyValueError::new_err("lock poisoned"))?;
            state.events.clone()
        };
        let range_size = self.range_size;

        let df = py
            .detach(move || {
                let (bars, has_all_ts) = compute_range_bars(&events, range_size)?;
                build_dataframe(&bars, has_all_ts)
            })
            .map_err(PyValueError::new_err)?;

        Ok(PyDataFrame(df))
    }

    fn reset(&self) -> PyResult<()> {
        let mut state = self
            .state
            .write()
            .map_err(|_| PyValueError::new_err("lock poisoned"))?;
        state.events.clear();
        state.next_seq = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price_event(price: f64, seq: u64) -> RawEvent {
        RawEvent {
            price: Some(price),
            datetime_ns: seq as i64,
            timestamp: Some(seq as i64),
            seq,
            kind: EventKind::Price { volume: 1.0 },
        }
    }

    #[test]
    fn matches_documented_price_sequence() {
        let prices = [
            100.0, 105.0, 110.0, 109.0, 112.0, 116.0, 119.0, 123.0, 127.0, 129.0, 135.0, 129.0,
            123.0, 118.0, 112.0, 118.0, 124.0, 119.0,
        ];
        let events: Vec<_> = prices
            .into_iter()
            .enumerate()
            .map(|(seq, price)| price_event(price, seq as u64))
            .collect();

        let (bars, has_timestamps) = compute_range_bars(&events, 10.0).unwrap();
        let values: Vec<_> = bars
            .iter()
            .map(|bar| (bar.open, bar.high, bar.low, bar.close))
            .collect();

        assert!(has_timestamps);
        assert_eq!(bars.iter().map(|bar| bar.tick_count).sum::<u64>(), 18);
        assert_eq!(bars.iter().map(|bar| bar.volume).sum::<f64>(), 18.0);
        assert_eq!(
            values,
            vec![
                (100.0, 110.0, 100.0, 110.0),
                (110.0, 120.0, 110.0, 120.0),
                (120.0, 130.0, 120.0, 130.0),
                (130.0, 135.0, 125.0, 125.0),
                (125.0, 125.0, 115.0, 115.0),
                (115.0, 122.0, 112.0, 122.0),
                (122.0, 124.0, 119.0, 119.0),
            ]
        );
    }

    #[test]
    fn ohlc_volume_is_split_across_every_generated_bar() {
        let event = RawEvent {
            price: None,
            datetime_ns: 1,
            timestamp: None,
            seq: 0,
            kind: EventKind::Ohlc {
                prices: [100.0, 100.0, 145.0, 145.0],
                volume: 11.0,
                is_int_volume: true,
            },
        };

        let (bars, _) = compute_range_bars(&[event], 10.0).unwrap();
        assert_eq!(bars.len(), 5);
        assert_eq!(
            bars.iter().map(|bar| bar.volume).collect::<Vec<_>>(),
            vec![2.0, 2.0, 2.0, 2.0, 3.0]
        );
        assert_eq!(bars.iter().map(|bar| bar.tick_count).sum::<u64>(), 4);
    }

    #[test]
    fn rejects_impossibly_large_output_before_allocating_it() {
        let events = vec![price_event(1.0, 0), price_event(2_000.0, 1)];
        let error = compute_range_bars(&events, 0.0001).unwrap_err();
        assert!(error.contains("safety limit"));
    }
}
