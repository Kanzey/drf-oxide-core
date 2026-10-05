use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyType};

/// Objects from Django / DRF that the core needs but must not import itself.
/// `drf_oxide` passes them once, at import time, through `configure()`.
pub struct Config {
    pub empty: Py<PyAny>,
    pub skip_field: Py<PyType>,
    pub object_does_not_exist: Py<PyType>,
    pub pk_only_object: Py<PyType>,
    pub manager_class: Py<PyType>,
    pub dict_factory: Py<PyAny>,
    pub dict_factory_is_dict: bool,
    /// `(value, attr) -> value`: calls `value()` when DRF's `is_simple_callable(value)` says so.
    pub resolve_callable: Py<PyAny>,
    /// `(field, data, validate_method) -> (status, value)`: the body of DRF's
    /// `Serializer.to_internal_value` loop for one field.
    pub run_field: Py<PyAny>,
    /// `(field, value, validate_method, run_validators) -> (status, value)`: the part of that
    /// loop which runs after `to_internal_value` succeeded natively.
    pub finish_field: Py<PyAny>,
}

pub const STATUS_OK: u8 = 0;
pub const STATUS_SKIP: u8 = 1;
pub const STATUS_ERROR: u8 = 2;

static CONFIG: PyOnceLock<Config> = PyOnceLock::new();

pub fn config(py: Python<'_>) -> PyResult<&'static Config> {
    CONFIG
        .get(py)
        .ok_or_else(|| PyRuntimeError::new_err("drf_oxide_core.configure() has not been called"))
}

impl Config {
    pub fn new_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        if self.dict_factory_is_dict {
            Ok(PyDict::new(py).into_any())
        } else {
            self.dict_factory.bind(py).call0()
        }
    }
}

#[pyfunction]
#[pyo3(signature = (
    *, empty, skip_field, object_does_not_exist, pk_only_object, manager_class,
    dict_factory, resolve_callable, run_field, finish_field
))]
#[allow(clippy::too_many_arguments)]
pub fn configure(
    py: Python<'_>,
    empty: Py<PyAny>,
    skip_field: Py<PyType>,
    object_does_not_exist: Py<PyType>,
    pk_only_object: Py<PyType>,
    manager_class: Py<PyType>,
    dict_factory: Py<PyAny>,
    resolve_callable: Py<PyAny>,
    run_field: Py<PyAny>,
    finish_field: Py<PyAny>,
) -> PyResult<()> {
    let dict_factory_is_dict = dict_factory.bind(py).is(py.get_type::<PyDict>());
    let cfg = Config {
        empty,
        skip_field,
        object_does_not_exist,
        pk_only_object,
        manager_class,
        dict_factory,
        dict_factory_is_dict,
        resolve_callable,
        run_field,
        finish_field,
    };
    CONFIG
        .set(py, cfg)
        .map_err(|_| PyRuntimeError::new_err("drf_oxide_core is already configured"))
}

#[pyfunction]
pub fn is_configured(py: Python<'_>) -> bool {
    CONFIG.get(py).is_some()
}
