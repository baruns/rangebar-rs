#[path = "rangebar.rs"]
mod rangebar_core;

use pyo3::prelude::*;

#[pymodule]
fn rangebar_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<rangebar_core::RangeBar>()?;
    Ok(())
}
