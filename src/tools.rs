use std::sync::Mutex;

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyList, PyString, PyType};

static DECIMAL: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static UUID: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static MAPPING: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static TIMEZONE: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static SAFE_UUID_UNKNOWN: PyOnceLock<Py<PyAny>> = PyOnceLock::new();

pub fn decimal_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    DECIMAL.import(py, "decimal", "Decimal")
}

pub fn uuid_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    UUID.import(py, "uuid", "UUID")
}

pub fn safe_uuid_unknown(py: Python<'_>) -> PyResult<&Bound<'_, PyAny>> {
    SAFE_UUID_UNKNOWN
        .get_or_try_init(py, || {
            Ok::<_, PyErr>(
                py.import("uuid")?
                    .getattr("SafeUUID")?
                    .getattr("unknown")?
                    .unbind(),
            )
        })
        .map(|v| v.bind(py))
}

/// `datetime.timezone`, what Django attaches to datetimes read from the database.
pub fn fixed_timezone_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    TIMEZONE.import(py, "datetime", "timezone")
}

pub fn mapping_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    MAPPING.import(py, "collections.abc", "Mapping")
}

/// Types recently seen not to be a `collections.abc.Mapping`. The ABC check costs ~200ns and runs
/// for every attribute of every serialized object, while those objects are nearly always of a
/// handful of model classes.
static NON_MAPPING_TYPES: Mutex<Vec<Py<PyType>>> = Mutex::new(Vec::new());
const NON_MAPPING_CACHE_SIZE: usize = 32;

/// `isinstance(obj, collections.abc.Mapping)`.
pub fn is_mapping(obj: &Bound<'_, PyAny>) -> PyResult<bool> {
    if obj.is_instance_of::<PyDict>() {
        return Ok(true);
    }
    let ty = obj.get_type();
    if let Ok(cache) = NON_MAPPING_TYPES.lock() {
        if cache.iter().any(|known| known.is(&ty)) {
            return Ok(false);
        }
    }
    let result = obj.is_instance(mapping_type(obj.py())?)?;
    if !result {
        if let Ok(mut cache) = NON_MAPPING_TYPES.lock() {
            if cache.len() >= NON_MAPPING_CACHE_SIZE {
                cache.remove(0);
            }
            cache.push(ty.unbind());
        }
    }
    Ok(result)
}

pub fn opt_item<'py>(d: &Bound<'py, PyDict>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    Ok(d.get_item(key)?.filter(|v| !v.is_none()))
}

pub fn req_item<'py>(d: &Bound<'py, PyDict>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    d.get_item(key)?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(format!("schema is missing '{key}'")))
}

pub fn opt_usize(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<usize>> {
    opt_item(d, key)?.map(|v| v.extract()).transpose()
}

pub fn opt_bool(d: &Bound<'_, PyDict>, key: &str, default: bool) -> PyResult<bool> {
    Ok(opt_item(d, key)?
        .map(|v| v.is_truthy())
        .transpose()?
        .unwrap_or(default))
}

pub fn str_list(obj: &Bound<'_, PyAny>) -> PyResult<Vec<Py<PyString>>> {
    let list = obj.cast::<PyList>()?;
    list.iter()
        .map(|item| Ok(PyString::intern(obj.py(), item.cast::<PyString>()?.to_str()?).unbind()))
        .collect()
}

/// Python's `str.isspace()` for a single character. Rust's `char::is_whitespace` misses the
/// ASCII information separators U+001C..U+001F.
pub fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` with no arguments.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

pub fn is_exact(obj: &Bound<'_, PyAny>, ty: &Bound<'_, PyType>) -> bool {
    obj.get_type().is(ty)
}
