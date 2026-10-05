use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyType};

use crate::config::Config;
use crate::tools::{is_mapping, opt_bool, opt_item, req_item, str_list};

/// One attribute of a `source` path.
pub enum Step {
    Attr(Py<PyString>),
    /// A Django relation on instances of exactly `model`. Its loaded value is read from the
    /// instance's caches, skipping the descriptor; anything not cached goes through `getattr`.
    Relation {
        name: Py<PyString>,
        model: Py<PyType>,
        kind: Relation,
    },
}

pub enum Relation {
    /// `ForwardManyToOneDescriptor` / `ForwardOneToOneDescriptor`: `_state.fields_cache`.
    Forward {
        cache_name: Py<PyString>,
        null: bool,
    },
    /// `ReverseOneToOneDescriptor`: `_state.fields_cache`, only when not `None`.
    ReverseOne { cache_name: Py<PyString> },
    /// Reverse FK / many-to-many with a prefetch: the list the related manager's `.all()` would
    /// iterate. Only used where the consumer calls `.all()` anyway.
    Many {
        cache_name: Py<PyString>,
        /// Attributes the related manager requires to be set (pk, related values).
        required: Vec<Py<PyString>>,
    },
}

/// How a readable field fetches its attribute from the instance.
pub enum Get {
    /// DRF's `get_attribute(instance, field.source_attrs)`.
    Attrs(Vec<Step>),
    /// `ManyRelatedField.get_attribute()` when the relation is prefetched.
    Many(Vec<Step>),
    /// `source='*'`: the instance itself.
    Star,
    /// `PrimaryKeyRelatedField` on a model: read `<fk>_id` directly, no query. With `from_dict`
    /// (a stock `DeferredAttribute`), a loaded value is taken from `instance.__dict__`, as the
    /// descriptor itself would do.
    PkAttname {
        attname: Py<PyString>,
        from_dict: bool,
    },
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
            "attrs" => Get::Attrs(parse_steps(&req_item(d, "attrs")?)?),
            "many" => Get::Many(parse_steps(&req_item(d, "attrs")?)?),
            "star" => Get::Star,
            "pk_attname" => {
                let name = req_item(d, "attname")?;
                Get::PkAttname {
                    attname: PyString::intern(d.py(), name.cast::<PyString>()?.to_str()?).unbind(),
                    from_dict: opt_bool(d, "from_dict", false)?,
                }
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
            Get::Attrs(steps) => match native_chain(cfg, instance, steps) {
                Some(value) => Ok(Got::Value(value)),
                None => python_get(cfg, field, instance),
            },
            Get::Many(steps) => match native_chain(cfg, instance, steps) {
                // A list here can only come from the prefetch cache: the descriptor returns a manager.
                Some(value) if value.is_exact_instance_of::<PyList>() => Ok(Got::Value(value)),
                _ => python_get(cfg, field, instance),
            },
            Get::PkAttname { attname, from_dict } => {
                let attname = attname.bind(py);
                if *from_dict {
                    if let Some(value) = instance_dict_item(instance, attname) {
                        return Ok(Got::Value(value));
                    }
                }
                match instance.getattr(attname) {
                    Ok(value) => Ok(Got::Value(value)),
                    Err(_) => python_get(cfg, field, instance),
                }
            }
            Get::Python => python_get(cfg, field, instance),
        }
    }

    pub fn is_python(&self) -> bool {
        matches!(self, Get::Python)
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Get::Attrs(_) => "attrs",
            Get::Many(_) => "many",
            Get::Star => "star",
            Get::PkAttname { .. } => "pk_attname",
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
    steps: &[Step],
) -> Option<Bound<'py, PyAny>> {
    let py = instance.py();
    let mut current = instance.clone();
    for step in steps {
        let attr = match step {
            Step::Attr(name) => name.bind(py),
            Step::Relation { name, model, kind } => {
                if current.get_type().is(model.bind(py)) {
                    if let Some(cached) = cached_relation(&current, kind) {
                        current = cached;
                        continue;
                    }
                }
                name.bind(py)
            }
        };
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

/// What the relation descriptor would return, when it would come from a cache. `None` when the
/// descriptor has to run (not loaded yet, or it would raise).
fn cached_relation<'py>(
    instance: &Bound<'py, PyAny>,
    kind: &Relation,
) -> Option<Bound<'py, PyAny>> {
    let py = instance.py();
    match kind {
        Relation::Forward { cache_name, null } => {
            let value = fields_cache(instance)?
                .get_item(cache_name.bind(py))
                .ok()??;
            // `RelatedObjectDoesNotExist` for a cached `None` on a non-null FK.
            (*null || !value.is_none()).then_some(value)
        }
        Relation::ReverseOne { cache_name } => {
            let value = fields_cache(instance)?
                .get_item(cache_name.bind(py))
                .ok()??;
            (!value.is_none()).then_some(value)
        }
        Relation::Many {
            cache_name,
            required,
        } => {
            for attname in required {
                if instance.getattr(attname.bind(py)).ok()?.is_none() {
                    return None;
                }
            }
            let cache = instance
                .getattr(intern!(py, "_prefetched_objects_cache"))
                .ok()?;
            let queryset = cache
                .cast::<PyDict>()
                .ok()?
                .get_item(cache_name.bind(py))
                .ok()??;
            let result = queryset.getattr(intern!(py, "_result_cache")).ok()?;
            if !result.is_exact_instance_of::<PyList>() {
                return None;
            }
            // Iterating the queryset would first run its own pending prefetches.
            let lookups = queryset
                .getattr(intern!(py, "_prefetch_related_lookups"))
                .ok()?;
            if lookups.is_truthy().ok()?
                && !queryset
                    .getattr(intern!(py, "_prefetch_done"))
                    .ok()?
                    .is_truthy()
                    .ok()?
            {
                return None;
            }
            Some(result)
        }
    }
}

fn instance_dict_item<'py>(
    instance: &Bound<'py, PyAny>,
    key: &Bound<'py, PyString>,
) -> Option<Bound<'py, PyAny>> {
    let dict = instance.getattr(intern!(instance.py(), "__dict__")).ok()?;
    dict.cast::<PyDict>().ok()?.get_item(key).ok()?
}

fn fields_cache<'py>(instance: &Bound<'py, PyAny>) -> Option<Bound<'py, PyDict>> {
    let py = instance.py();
    let state = instance.getattr(intern!(py, "_state")).ok()?;
    state
        .getattr(intern!(py, "fields_cache"))
        .ok()?
        .cast_into::<PyDict>()
        .ok()
}

fn parse_steps(obj: &Bound<'_, PyAny>) -> PyResult<Vec<Step>> {
    let py = obj.py();
    let intern_str = |v: Bound<'_, PyAny>| -> PyResult<Py<PyString>> {
        Ok(PyString::intern(py, v.cast::<PyString>()?.to_str()?).unbind())
    };
    obj.cast::<PyList>()?
        .iter()
        .map(|item| {
            let Ok(d) = item.cast::<PyDict>() else {
                return Ok(Step::Attr(intern_str(item)?));
            };
            let kind: String = req_item(d, "kind")?.extract()?;
            let cache_name = intern_str(req_item(d, "cache_name")?)?;
            let kind = match kind.as_str() {
                "forward" => Relation::Forward {
                    cache_name,
                    null: opt_bool(d, "null", false)?,
                },
                "reverse_one" => Relation::ReverseOne { cache_name },
                "many" => Relation::Many {
                    cache_name,
                    required: str_list(&req_item(d, "required")?)?,
                },
                other => {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "unknown relation kind '{other}'"
                    )))
                }
            };
            Ok(Step::Relation {
                name: intern_str(req_item(d, "name")?)?,
                model: req_item(d, "model")?.cast_into::<PyType>()?.unbind(),
                kind,
            })
        })
        .collect()
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
