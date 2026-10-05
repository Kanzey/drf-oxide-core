use pyo3::prelude::*;

mod config;
mod format;
mod getattr;
mod json;
mod repr;
mod serializer;
mod tools;
mod validate;

#[pymodule]
fn _drf_oxide_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("STATUS_OK", config::STATUS_OK)?;
    m.add("STATUS_SKIP", config::STATUS_SKIP)?;
    m.add("STATUS_ERROR", config::STATUS_ERROR)?;
    m.add_class::<serializer::CompiledSerializer>()?;
    m.add("JsonFallback", m.py().get_type::<json::JsonFallback>())?;
    m.add_function(wrap_pyfunction!(config::configure, m)?)?;
    m.add_function(wrap_pyfunction!(config::is_configured, m)?)?;
    m.add_function(wrap_pyfunction!(json::to_json, m)?)?;
    m.add_function(wrap_pyfunction!(json::from_json, m)?)?;
    Ok(())
}
