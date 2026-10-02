use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};

use crate::config::Config;
use crate::tools::{is_mapping, opt_item, req_item, str_list};

/// How a readable field fetches its attribute from the instance.
pub enum Get {
    /// DRF's `get_attribute(instance, field.source_attrs)`.
    Attrs(Vec<Py<PyString>>),
    /// `source='*'`: the instance itself.
    Star,
    /// `PrimaryKeyRelatedField` on a model: read `<fk>_id` directly, no query.
    PkAttname(Py<PyString>),
    /// `field.get_attribute(instance)` in Python.
    Python,
}

pub enum Got<'py> {
    Value(Bound<'py, PyAny>),
    Skip,
}

impl Get {
    pub fn parse(d: &Bound<'_, PyDict>) -> PyResult<Self> {
        let kind: String = req_item(d, "type")?.extract()?;
        Ok(match kind.as_str() {
            "attrs" => Get::Attrs(str_list(&req_item(d, "attrs")?)?),
            "star" => Get::Star,
            "pk_attname" => {
                let name = req_item(d, "attname")?;
                Get::PkAttname(
                    PyString::intern(d.py(), name.cast::<PyString>()?.to_str()?).unbind(),
                )
            }
            "python" => Get::Python,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown get type '{other}'"
                )));
            }
        })
    }

    pub fn get<'py>(
        &self,
        cfg: &Config,
        field: &Bound<'py, PyAny>,
        instance: &Bound<'py, PyAny>,
    ) -> PyResult<Got<'py>> {
        let py = instance.py();
        match self {
            Get::Star => Ok(Got::Value(instance.clone())),
            Get::Attrs(attrs) => match native_chain(cfg, instance, attrs) {
                Some(value) => Ok(Got::Value(value)),
                None => python_get(cfg, field, instance),
            },
            Get::PkAttname(attname) => match instance.getattr(attname.bind(py)) {
                Ok(value) => Ok(Got::Value(value)),
                Err(_) => python_get(cfg, field, instance),
            },
            Get::Python => python_get(cfg, field, instance),
        }
    }

    pub fn is_python(&self) -> bool {
        matches!(self, Get::Python)
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Get::Attrs(_) => "attrs",
            Get::Star => "star",
            Get::PkAttname(_) => "pk_attname",
            Get::Python => "python",
        }
    }
}

/// `rest_framework.fields.get_attribute` for the common cases. `None` means "something unusual
/// happened, let `field.get_attribute()` redo it in Python", so DRF's error handling (defaults,
/// `SkipField`, re-raised messages) stays exactly the same.
fn native_chain<'py>(
    cfg: &Config,
    instance: &Bound<'py, PyAny>,
    attrs: &[Py<PyString>],
) -> Option<Bound<'py, PyAny>> {
    let py = instance.py();
    let mut current = instance.clone();
    for attr in attrs {
        let attr = attr.bind(py);
        let next = if let Ok(dict) = current.cast_exact::<PyDict>() {
            match dict.get_item(attr) {
                Ok(Some(value)) => Ok(value),
                _ => return None,
            }
        } else {
            match is_mapping(&current) {
                Ok(true) => current.get_item(attr),
                Ok(false) => current.getattr(attr),
                Err(_) => return None,
            }
        };
        current = match next {
            Ok(value) => value,
            Err(err) if err.is_instance(py, cfg.object_does_not_exist.bind(py)) => {
                return Some(py.None().into_bound(py))
            }
            Err(_) => return None,
        };
        if current.is_callable() {
            current = cfg.resolve_callable.bind(py).call1((current, attr)).ok()?;
        }
    }
    Some(current)
}

fn python_get<'py>(
    cfg: &Config,
    field: &Bound<'py, PyAny>,
    instance: &Bound<'py, PyAny>,
) -> PyResult<Got<'py>> {
    let py = instance.py();
    match field.call_method1(pyo3::intern!(py, "get_attribute"), (instance,)) {
        Ok(value) => Ok(Got::Value(value)),
        Err(err) if err.is_instance(py, cfg.skip_field.bind(py)) => Ok(Got::Skip),
        Err(err) => Err(err),
    }
}

pub fn parse_get(d: &Bound<'_, PyDict>) -> PyResult<Get> {
    match opt_item(d, "get")? {
        Some(get) => Get::parse(get.cast::<PyDict>()?),
        None => Ok(Get::Python),
    }
}
