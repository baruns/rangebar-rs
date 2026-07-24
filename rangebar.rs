use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use polars::prelude::*;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3_polars::PyDataFrame;

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
        row_id: u64,
        row_volume: f64,
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
    price: f64,
    datetime_ns: i64,
    timestamp: Option<i64>,
    seq: u64,
    kind: EventKind,
}

struct InnerState {
    events: Vec<RawEvent>,
    next_seq: u64,
    next_row_id: u64,
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
    ohlc_rows: HashMap<u64, (f64, bool)>,
}

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

fn compute_range_bars(events: &[RawEvent], range_size: f64) -> (Vec<OutputBar>, bool) {
    let has_all_timestamps = !events.is_empty() && events.iter().all(|e| e.timestamp.is_some());

    if events.is_empty() {
        return (Vec::new(), has_all_timestamps);
    }

    let mut completed_bars: Vec<BarBuilder> = Vec::new();
    let mut current: Option<BarBuilder> = None;

    for event in events {
        let price = event.price;

        match current {
            None => {
                let mut bar = BarBuilder {
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                    volume: 0.0,
                    tick_count: 1,
                    datetime_start: event.datetime_ns,
                    datetime_end: event.datetime_ns,
                    timestamp_start: event.timestamp,
                    timestamp_end: event.timestamp,
                    ohlc_rows: HashMap::new(),
                };
                match &event.kind {
                    EventKind::Ohlc {
                        row_id,
                        row_volume,
                        is_int_volume,
                    } => {
                        bar.ohlc_rows.insert(*row_id, (*row_volume, *is_int_volume));
                    }
                    EventKind::Price { volume } | EventKind::Single { volume } => {
                        bar.volume += volume;
                    }
                }
                current = Some(bar);
            }
            Some(ref mut bar) => loop {
                let upper = round4(bar.open + range_size);
                let lower = round4(bar.open - range_size);

                if price > upper {
                    bar.high = upper;
                    bar.close = upper;
                    bar.datetime_end = event.datetime_ns;
                    bar.timestamp_end = event.timestamp;
                    let finished = bar.clone();
                    completed_bars.push(finished);

                    *bar = BarBuilder {
                        open: upper,
                        high: upper,
                        low: upper,
                        close: upper,
                        volume: 0.0,
                        tick_count: 0,
                        datetime_start: event.datetime_ns,
                        datetime_end: event.datetime_ns,
                        timestamp_start: event.timestamp,
                        timestamp_end: event.timestamp,
                        ohlc_rows: HashMap::new(),
                    };
                } else if price < lower {
                    bar.low = lower;
                    bar.close = lower;
                    bar.datetime_end = event.datetime_ns;
                    bar.timestamp_end = event.timestamp;
                    let finished = bar.clone();
                    completed_bars.push(finished);

                    *bar = BarBuilder {
                        open: lower,
                        high: lower,
                        low: lower,
                        close: lower,
                        volume: 0.0,
                        tick_count: 0,
                        datetime_start: event.datetime_ns,
                        datetime_end: event.datetime_ns,
                        timestamp_start: event.timestamp,
                        timestamp_end: event.timestamp,
                        ohlc_rows: HashMap::new(),
                    };
                } else {
                    bar.high = bar.high.max(price);
                    bar.low = bar.low.min(price);
                    bar.close = price;
                    bar.tick_count += 1;
                    bar.datetime_end = event.datetime_ns;
                    bar.timestamp_end = event.timestamp;

                    match &event.kind {
                        EventKind::Ohlc {
                            row_id,
                            row_volume,
                            is_int_volume,
                        } => {
                            bar.ohlc_rows
                                .entry(*row_id)
                                .or_insert((*row_volume, *is_int_volume));
                        }
                        EventKind::Price { volume } | EventKind::Single { volume } => {
                            bar.volume += volume;
                        }
                    }
                    break;
                }
            },
        }
    }

    let mut all_bars = completed_bars;
    if let Some(bar) = current {
        all_bars.push(bar);
    }

    let mut row_bar_indices: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, bar) in all_bars.iter().enumerate() {
        for row_id in bar.ohlc_rows.keys() {
            row_bar_indices.entry(*row_id).or_default().push(i);
        }
    }

    for (row_id, bar_indices) in &row_bar_indices {
        let (row_volume, is_int) = all_bars[bar_indices[0]].ohlc_rows[row_id];
        let n = bar_indices.len();

        if n == 1 {
            all_bars[bar_indices[0]].volume += row_volume;
        } else {
            let portion = if is_int {
                (row_volume / n as f64).round()
            } else {
                row_volume / n as f64
            };

            let mut allocated = 0.0;
            for (j, bar_idx) in bar_indices.iter().enumerate() {
                if j < n - 1 {
                    all_bars[*bar_idx].volume += portion;
                    allocated += portion;
                } else {
                    all_bars[*bar_idx].volume += row_volume - allocated;
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

    (output, has_all_timestamps)
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
        if round4(*v) <= 0.0 {
            return Err(PyValueError::new_err(format!("{} must be > 0", col_name)));
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

fn append_events_and_sort(state: &mut InnerState, new_events: Vec<RawEvent>) {
    let needs_sort = if let Some(last) = state.events.last() {
        new_events.iter().any(|e| e.datetime_ns < last.datetime_ns)
    } else {
        false
    };
    state.events.extend(new_events);
    if needs_sort {
        state
            .events
            .sort_by(|a, b| a.datetime_ns.cmp(&b.datetime_ns).then(a.seq.cmp(&b.seq)));
    }
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
                next_row_id: 0,
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
            price,
            datetime_ns,
            timestamp,
            seq,
            kind: EventKind::Single { volume: ltq },
        };

        append_events_and_sort(&mut state, vec![event]);
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
            let mut events = Vec::with_capacity(n);
            for i in 0..n {
                let ts = timestamps.as_ref().map(|t| t[i]);
                events.push(RawEvent {
                    price: prices[i],
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
        append_events_and_sort(&mut state, new_events);
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
        let highs = extract_price_col(&high_series, "high")?;
        let lows = extract_price_col(&low_series, "low")?;
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
            let h = highs[i];
            let l = lows[i];
            let c = closes[i];
            if h < o.max(c).max(l) {
                return Err(PyValueError::new_err(format!(
                    "row {}: high must be >= max(open, close, low)",
                    i
                )));
            }
            if l > o.min(c).min(h) {
                return Err(PyValueError::new_err(format!(
                    "row {}: low must be <= min(open, close, high)",
                    i
                )));
            }
        }

        let state_arc = self.state.clone();
        let result: Result<Vec<RawEvent>, String> = py.detach(move || {
            let mut state = state_arc.write().map_err(|_| "lock poisoned".to_string())?;
            let mut events = Vec::with_capacity(n * 4);
            for i in 0..n {
                let o = opens[i];
                let h = highs[i];
                let l = lows[i];
                let c = closes[i];
                let dt = datetimes[i];
                let ts = timestamps.as_ref().map(|t| t[i]);
                let vol = volumes[i];
                let row_id = state.next_row_id;
                state.next_row_id += 1;

                let tick_prices = if c >= o { [o, l, h, c] } else { [o, h, l, c] };

                for tp in &tick_prices {
                    events.push(RawEvent {
                        price: *tp,
                        datetime_ns: dt,
                        timestamp: ts,
                        seq: state.next_seq,
                        kind: EventKind::Ohlc {
                            row_id,
                            row_volume: vol,
                            is_int_volume,
                        },
                    });
                    state.next_seq += 1;
                }
            }
            Ok(events)
        });

        let new_events = result.map_err(PyValueError::new_err)?;

        let mut state = self
            .state
            .write()
            .map_err(|_| PyValueError::new_err("lock poisoned"))?;
        append_events_and_sort(&mut state, new_events);
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
                let (bars, has_all_ts) = compute_range_bars(&events, range_size);
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
        state.next_row_id = 0;
        Ok(())
    }
}
