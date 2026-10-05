use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyTuple};

use crate::config::{config, Config, STATUS_ERROR, STATUS_OK};
use crate::getattr::{parse_get, Get, Got};
use crate::repr::{Ctx, Repr};
use crate::tools::{opt_bool, opt_item, req_item, str_list};
use crate::validate::Val;

struct ReadField {
    name: Py<PyString>,
    field: Py<PyAny>,
    get: Get,
    repr: Repr,
}

struct WriteField {
    name: Py<PyString>,
    field: Py<PyAny>,
    /// Empty for `source='*'`.
    source_attrs: Vec<Py<PyString>>,
    validate_method: Option<Py<PyAny>>,
    allow_null: bool,
    /// Validators that are not covered by `val` and must run in Python.
    python_validators: bool,
    val: Val,
}

/// A DRF serializer instance compiled to native code.
///
/// It holds the live field objects of one serializer instance (fields know their `parent`,
/// `context` and `root`), so it must be built per serializer instance, not per class.
#[pyclass(frozen, module = "drf_oxide_core._drf_oxide_core")]
pub struct CompiledSerializer {
    read: Vec<ReadField>,
    write: Vec<WriteField>,
}

#[pymethods]
impl CompiledSerializer {
    #[new]
    #[pyo3(signature = (read_fields, write_fields))]
    fn new(read_fields: &Bound<'_, PyList>, write_fields: &Bound<'_, PyList>) -> PyResult<Self> {
        let py = read_fields.py();
        let read = read_fields
            .iter()
            .map(|item| {
                let d = item.cast::<PyDict>()?;
                Ok(ReadField {
                    name: field_name(py, d)?,
                    field: req_item(d, "field")?.unbind(),
                    get: parse_get(d)?,
                    repr: Repr::parse(req_item(d, "repr")?.cast::<PyDict>()?)?,
                })
            })
            .collect::<PyResult<Vec<_>>>()?;
        let write = write_fields
            .iter()
            .map(|item| {
                let d = item.cast::<PyDict>()?;
                Ok(WriteField {
                    name: field_name(py, d)?,
                    field: req_item(d, "field")?.unbind(),
                    source_attrs: str_list(&req_item(d, "source_attrs")?)?,
                    validate_method: opt_item(d, "validate_method")?.map(Bound::unbind),
                    allow_null: opt_bool(d, "allow_null", false)?,
                    python_validators: opt_bool(d, "python_validators", false)?,
                    val: Val::parse(req_item(d, "val")?.cast::<PyDict>()?)?,
                })
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self { read, write })
    }

    /// `Serializer.to_representation(instance)`.
    #[pyo3(signature = (instance, current_tz=None))]
    fn to_representation<'py>(
        &self,
        instance: &Bound<'py, PyAny>,
        current_tz: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = instance.py();
        let ctx = Ctx {
            py,
            cfg: config(py)?,
            current_tz,
        };
        self.repr_one(&ctx, instance)
    }

    /// `[Serializer.to_representation(item) for item in iterable]`.
    #[pyo3(signature = (iterable, current_tz=None))]
    fn to_representation_many<'py>(
        &self,
        iterable: &Bound<'py, PyAny>,
        current_tz: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyList>> {
        let py = iterable.py();
        let ctx = Ctx {
            py,
            cfg: config(py)?,
            current_tz,
        };
        self.repr_many(&ctx, iterable)
    }

    /// The field loop of `Serializer.to_internal_value(data)` for a plain `dict`.
    /// Returns `(validated, errors)`; `errors` is `None` when everything validated.
    #[pyo3(signature = (data, current_tz=None))]
    fn to_internal_value<'py>(
        &self,
        data: &Bound<'py, PyDict>,
        current_tz: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyTuple>> {
        let py = data.py();
        let (ret, errors) = match self.validate_dict(data, current_tz.as_ref())? {
            Ok(ret) => (ret, py.None().into_bound(py)),
            Err((ret, errors)) => (ret, errors),
        };
        PyTuple::new(py, [ret, errors])
    }

    /// Which strategy each field got: `{'read': {name: (get, repr)}, 'write': {name: (val, python_validators)}}`.
    fn describe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let read = PyDict::new(py);
        for rf in &self.read {
            read.set_item(rf.name.bind(py), (rf.get.kind(), rf.repr.kind()))?;
        }
        let write = PyDict::new(py);
        for wf in &self.write {
            write.set_item(wf.name.bind(py), (wf.val.kind(), wf.python_validators))?;
        }
        let out = PyDict::new(py);
        out.set_item("read", read)?;
        out.set_item("write", write)?;
        Ok(out)
    }

    fn __repr__(&self) -> String {
        format!(
            "CompiledSerializer(read_fields={}, write_fields={})",
            self.read.len(),
            self.write.len()
        )
    }
}

fn field_name(py: Python<'_>, d: &Bound<'_, PyDict>) -> PyResult<Py<PyString>> {
    let name = req_item(d, "name")?;
    Ok(PyString::intern(py, name.cast::<PyString>()?.to_str()?).unbind())
}

type Validated<'py> = Result<Bound<'py, PyAny>, (Bound<'py, PyAny>, Bound<'py, PyAny>)>;

impl CompiledSerializer {
    /// `Ok(validated)` or `Err((partial, errors))`.
    pub fn validate_dict<'py>(
        &self,
        data: &Bound<'py, PyDict>,
        current_tz: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Validated<'py>> {
        let py = data.py();
        let cfg = config(py)?;
        let ret = cfg.new_dict(py)?;
        let mut errors: Option<Bound<'py, PyAny>> = None;

        for wf in &self.write {
            let name = wf.name.bind(py);
            let field = wf.field.bind(py);
            let outcome = match data.get_item(name)? {
                None => run_field(cfg, wf, field, data)?,
                Some(raw) => match native_value(wf, &raw, current_tz)? {
                    Some((value, validated)) => {
                        let run_validators = validated && wf.python_validators;
                        if run_validators || wf.validate_method.is_some() {
                            finish_field(cfg, wf, field, value, run_validators)?
                        } else {
                            Outcome::Ok(value)
                        }
                    }
                    None => run_field(cfg, wf, field, data)?,
                },
            };
            match outcome {
                Outcome::Ok(value) => set_value(&ret, &wf.source_attrs, value)?,
                Outcome::Skip => {}
                Outcome::Error(detail) => {
                    let errs = match &errors {
                        Some(e) => e,
                        None => errors.insert(cfg.new_dict(py)?),
                    };
                    errs.set_item(name, detail)?;
                }
            }
        }
        match errors {
            None => Ok(Ok(ret)),
            Some(errors) => Ok(Err((ret, errors))),
        }
    }

    pub fn repr_one<'py>(
        &self,
        ctx: &Ctx<'py>,
        instance: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = ctx.py;
        let ret = ctx.cfg.new_dict(py)?;
        for rf in &self.read {
            let field = rf.field.bind(py);
            let attribute = match rf.get.get(ctx.cfg, field, instance)? {
                Got::Value(value) => value,
                Got::Skip => continue,
            };
            let value = if is_none_attribute(ctx.cfg, &rf.get, &attribute)? {
                attribute
            } else {
                rf.repr.to_repr(ctx, field, attribute)?
            };
            ret.set_item(rf.name.bind(py), value)?;
        }
        Ok(ret)
    }

    pub fn repr_many<'py>(
        &self,
        ctx: &Ctx<'py>,
        iterable: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyList>> {
        let out = PyList::empty(ctx.py);
        for item in iterable.try_iter()? {
            out.append(self.repr_one(ctx, &item?)?)?;
        }
        Ok(out)
    }
}

/// DRF's `check_for_none`: `PKOnlyObject` is `None` when its `pk` is. Only `field.get_attribute()`
/// in Python can return a `PKOnlyObject`. For it we keep the original object, as DRF stores `None`.
fn is_none_attribute(cfg: &Config, get: &Get, attribute: &Bound<'_, PyAny>) -> PyResult<bool> {
    if attribute.is_none() {
        return Ok(true);
    }
    let py = attribute.py();
    if get.is_python() && attribute.is_instance(cfg.pk_only_object.bind(py))? {
        return Ok(attribute.getattr(intern!(py, "pk"))?.is_none());
    }
    Ok(false)
}

enum Outcome<'py> {
    Ok(Bound<'py, PyAny>),
    Skip,
    Error(Bound<'py, PyAny>),
}

/// Native counterpart of `field.run_validation(raw)`. The bool says whether `to_internal_value`
/// ran (so the field's Python validators still have to run), which is false for an accepted `None`.
fn native_value<'py>(
    wf: &WriteField,
    raw: &Bound<'py, PyAny>,
    current_tz: Option<&Bound<'py, PyAny>>,
) -> PyResult<Option<(Bound<'py, PyAny>, bool)>> {
    if wf.val.is_python() {
        return Ok(None);
    }
    if raw.is_none() {
        // validate_empty_values: `(True, None)`, except for nullable `source='*'` fields.
        return Ok((wf.allow_null && !wf.source_attrs.is_empty()).then(|| (raw.clone(), false)));
    }
    let run_validators = !wf.val.skips_validators(raw);
    Ok(wf
        .val
        .validate(raw, current_tz)?
        .map(|value| (value, run_validators)))
}

fn run_field<'py>(
    cfg: &Config,
    wf: &WriteField,
    field: &Bound<'py, PyAny>,
    data: &Bound<'py, PyDict>,
) -> PyResult<Outcome<'py>> {
    let py = data.py();
    let method = wf.validate_method.as_ref().map(|m| m.bind(py).clone());
    let result = cfg.run_field.bind(py).call1((field, data, method))?;
    outcome(result)
}

fn finish_field<'py>(
    cfg: &Config,
    wf: &WriteField,
    field: &Bound<'py, PyAny>,
    value: Bound<'py, PyAny>,
    run_validators: bool,
) -> PyResult<Outcome<'py>> {
    let py = field.py();
    let method = wf.validate_method.as_ref().map(|m| m.bind(py).clone());
    let result = cfg
        .finish_field
        .bind(py)
        .call1((field, value, method, run_validators))?;
    outcome(result)
}

fn outcome(result: Bound<'_, PyAny>) -> PyResult<Outcome<'_>> {
    let (status, value): (u8, Bound<'_, PyAny>) = result.extract()?;
    Ok(match status {
        STATUS_OK => Outcome::Ok(value),
        STATUS_ERROR => Outcome::Error(value),
        _ => Outcome::Skip,
    })
}

/// `rest_framework.fields.set_value`.
fn set_value<'py>(
    ret: &Bound<'py, PyAny>,
    keys: &[Py<PyString>],
    value: Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = ret.py();
    let Some((last, parents)) = keys.split_last() else {
        ret.call_method1(intern!(py, "update"), (value,))?;
        return Ok(());
    };
    let mut target = ret.clone();
    for key in parents {
        let key = key.bind(py);
        target = match target.get_item(key) {
            Ok(existing) => existing,
            Err(_) => {
                let created = PyDict::new(py).into_any();
                target.set_item(key, &created)?;
                created
            }
        };
    }
    target.set_item(last.bind(py), value)
}
