use pyo3::exceptions::PyValueError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{
    PyBool, PyDate, PyDateTime, PyDict, PyFloat, PyInt, PyList, PyString, PyTime, PyTzInfoAccess,
};

use crate::config::Config;
use crate::format::{date_iso, datetime_iso, utcoffset, uuid_str};
use crate::serializer::CompiledSerializer;
use crate::tools::{
    decimal_type, fixed_timezone_type, is_exact, opt_bool, opt_item, opt_usize, req_item, uuid_type,
};

/// Per-call state shared by the whole serializer tree.
pub struct Ctx<'py> {
    pub py: Python<'py>,
    pub cfg: &'static Config,
    /// `timezone.get_current_timezone()` when `USE_TZ`, resolved once per call.
    pub current_tz: Option<Bound<'py, PyAny>>,
}

pub enum DateTimeTz {
    /// The field has an explicit `default_timezone`.
    Fixed(Py<PyAny>),
    /// `settings.USE_TZ`: use the timezone active for the request.
    Current,
    /// `USE_TZ = False`: datetimes stay naive.
    Naive,
}

impl DateTimeTz {
    pub fn parse(d: &Bound<'_, PyDict>) -> PyResult<Self> {
        Ok(match opt_item(d, "timezone")? {
            Some(tz) => DateTimeTz::Fixed(tz.unbind()),
            None if opt_bool(d, "use_tz", false)? => DateTimeTz::Current,
            None => DateTimeTz::Naive,
        })
    }
}

/// How a readable field turns its attribute into primitive data. Every native variant only
/// handles the exact types it knows; anything else goes to `field.to_representation()`.
pub enum Repr {
    Passthrough,
    Str,
    Int,
    Float,
    Bool,
    Decimal {
        decimal_places: Option<usize>,
        max_digits: Option<usize>,
        coerce_to_string: bool,
    },
    Date,
    DateTime(DateTimeTz),
    Time,
    Uuid,
    Choice(Py<PyDict>),
    List(Box<Repr>, Py<PyAny>),
    Dict(Box<Repr>, Py<PyAny>),
    Nested(Py<CompiledSerializer>),
    NestedMany(Py<CompiledSerializer>),
    /// The attribute already is the primary key (see `Get::PkAttname`).
    Pk,
    /// `ManyRelatedField` around a plain `PrimaryKeyRelatedField`: `[obj.pk for obj in value]`.
    PkMany,
    Method(Py<PyAny>),
    /// DRF's `ModelField` (custom model fields): `getattr(obj, attname)` when it is a number or
    /// `None`, the field's own `to_representation(obj)` otherwise.
    ModelAttr(Py<PyString>),
    Python,
}

impl Repr {
    pub fn parse(d: &Bound<'_, PyDict>) -> PyResult<Self> {
        let kind: String = req_item(d, "type")?.extract()?;
        Ok(match kind.as_str() {
            "passthrough" => Repr::Passthrough,
            "str" => Repr::Str,
            "int" => Repr::Int,
            "float" => Repr::Float,
            "bool" => Repr::Bool,
            "decimal" => Repr::Decimal {
                decimal_places: opt_usize(d, "decimal_places")?,
                max_digits: opt_usize(d, "max_digits")?,
                coerce_to_string: opt_bool(d, "coerce_to_string", true)?,
            },
            "date" => Repr::Date,
            "datetime" => Repr::DateTime(DateTimeTz::parse(d)?),
            "time" => Repr::Time,
            "uuid" => Repr::Uuid,
            "choice" => Repr::Choice(req_item(d, "map")?.cast_into::<PyDict>()?.unbind()),
            "list" | "dict" => {
                let child = Box::new(Repr::parse(req_item(d, "child")?.cast::<PyDict>()?)?);
                let child_field = req_item(d, "child_field")?.unbind();
                if kind == "list" {
                    Repr::List(child, child_field)
                } else {
                    Repr::Dict(child, child_field)
                }
            }
            "nested" => {
                let serializer = req_item(d, "serializer")?
                    .cast_into::<CompiledSerializer>()?
                    .unbind();
                if opt_bool(d, "many", false)? {
                    Repr::NestedMany(serializer)
                } else {
                    Repr::Nested(serializer)
                }
            }
            "pk" => Repr::Pk,
            "pk_many" => Repr::PkMany,
            "method" => Repr::Method(req_item(d, "method")?.unbind()),
            "model_attr" => {
                let attname = req_item(d, "attname")?;
                Repr::ModelAttr(
                    PyString::intern(d.py(), attname.cast::<PyString>()?.to_str()?).unbind(),
                )
            }
            "python" => Repr::Python,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown repr type '{other}'"
                )))
            }
        })
    }

    pub fn kind(&self) -> String {
        match self {
            Repr::Passthrough => "passthrough".into(),
            Repr::Str => "str".into(),
            Repr::Int => "int".into(),
            Repr::Float => "float".into(),
            Repr::Bool => "bool".into(),
            Repr::Decimal { .. } => "decimal".into(),
            Repr::Date => "date".into(),
            Repr::DateTime(_) => "datetime".into(),
            Repr::Time => "time".into(),
            Repr::Uuid => "uuid".into(),
            Repr::Choice(_) => "choice".into(),
            Repr::List(child, _) => format!("list[{}]", child.kind()),
            Repr::Dict(child, _) => format!("dict[{}]", child.kind()),
            Repr::Nested(_) => "nested".into(),
            Repr::NestedMany(_) => "nested_many".into(),
            Repr::Pk => "pk".into(),
            Repr::PkMany => "pk_many".into(),
            Repr::Method(_) => "method".into(),
            Repr::ModelAttr(_) => "model_attr".into(),
            Repr::Python => "python".into(),
        }
    }

    /// `field.to_representation(value)` for a value that is not `None`.
    pub fn to_repr<'py>(
        &self,
        ctx: &Ctx<'py>,
        field: &Bound<'py, PyAny>,
        value: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = ctx.py;
        let native = match self {
            Repr::Passthrough | Repr::Pk => return Ok(value),
            Repr::Str => value
                .is_exact_instance_of::<PyString>()
                .then(|| value.clone()),
            Repr::Int => value.is_exact_instance_of::<PyInt>().then(|| value.clone()),
            Repr::Float => value
                .is_exact_instance_of::<PyFloat>()
                .then(|| value.clone()),
            Repr::Bool => value
                .is_exact_instance_of::<PyBool>()
                .then(|| value.clone()),
            Repr::Decimal {
                decimal_places,
                max_digits,
                coerce_to_string,
            } => decimal_repr(&value, *decimal_places, *max_digits, *coerce_to_string)?,
            Repr::Date => value
                .cast_exact::<PyDate>()
                .ok()
                .map(|date| PyString::new(py, &date_iso(date)).into_any()),
            Repr::DateTime(tz) => datetime_repr(ctx, tz, &value)?,
            Repr::Time => {
                if value.is_exact_instance_of::<PyTime>() {
                    Some(value.call_method0(intern!(py, "isoformat"))?)
                } else {
                    None
                }
            }
            Repr::Uuid => {
                if is_exact(&value, uuid_type(py)?) {
                    Some(uuid_str(&value)?.into_any())
                } else {
                    None
                }
            }
            Repr::Choice(map) => choice_repr(map.bind(py), &value)?,
            Repr::List(child, child_field) => {
                let child_field = child_field.bind(py);
                let out = PyList::empty(py);
                for item in value.try_iter()? {
                    let item = item?;
                    if item.is_none() {
                        out.append(item)?;
                    } else {
                        out.append(child.to_repr(ctx, child_field, item)?)?;
                    }
                }
                Some(out.into_any())
            }
            Repr::Dict(child, child_field) => {
                let child_field = child_field.bind(py);
                let out = PyDict::new(py);
                for item in value.call_method0(intern!(py, "items"))?.try_iter()? {
                    let (key, val): (Bound<'py, PyAny>, Bound<'py, PyAny>) = item?.extract()?;
                    let key = key.str()?;
                    if val.is_none() {
                        out.set_item(key, val)?;
                    } else {
                        out.set_item(key, child.to_repr(ctx, child_field, val)?)?;
                    }
                }
                Some(out.into_any())
            }
            Repr::Nested(serializer) => Some(serializer.get().repr_one(ctx, &value)?),
            Repr::NestedMany(serializer) => {
                let iterable = if value.is_instance(ctx.cfg.manager_class.bind(py))? {
                    value.call_method0(intern!(py, "all"))?
                } else {
                    value.clone()
                };
                Some(serializer.get().repr_many(ctx, &iterable)?.into_any())
            }
            Repr::PkMany => {
                let out = PyList::empty(py);
                for item in value.try_iter()? {
                    out.append(item?.getattr(intern!(py, "pk"))?)?;
                }
                Some(out.into_any())
            }
            Repr::Method(method) => return method.bind(py).call1((value,)),
            Repr::ModelAttr(attname) => {
                let attr = value.getattr(attname.bind(py))?;
                (attr.is_none()
                    || attr.is_instance_of::<PyInt>()
                    || attr.is_instance_of::<PyFloat>())
                .then_some(attr)
            }
            Repr::Python => {
                return field.call_method1(intern!(py, "to_representation"), (value,));
            }
        };
        match native {
            Some(result) => Ok(result),
            None => field.call_method1(intern!(py, "to_representation"), (value,)),
        }
    }
}

/// `DecimalField.to_representation` when `str(value)` already is the quantized form: the usual
/// case for values read from a `DecimalField` column. Returns `None` when rounding would be needed.
fn decimal_repr<'py>(
    value: &Bound<'py, PyAny>,
    decimal_places: Option<usize>,
    max_digits: Option<usize>,
    coerce_to_string: bool,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = value.py();
    if !is_exact(value, decimal_type(py)?) {
        return Ok(None);
    }
    let text = value.str()?;
    let s = text.to_str()?;
    let digits = s.strip_prefix('-').unwrap_or(s);
    let (int_part, frac_part) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    if int_part.is_empty()
        || !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        // Exponent notation, NaN, Infinity.
        return Ok(None);
    }
    if let Some(places) = decimal_places {
        if frac_part.len() != places {
            return Ok(None);
        }
        if let Some(max) = max_digits {
            let coefficient = format!("{int_part}{frac_part}");
            let significant = coefficient.trim_start_matches('0').len().max(1);
            if significant > max {
                return Ok(None);
            }
        }
    }
    if coerce_to_string {
        Ok(Some(text.into_any()))
    } else {
        Ok(Some(value.clone()))
    }
}

fn datetime_repr<'py>(
    ctx: &Ctx<'py>,
    tz: &DateTimeTz,
    value: &Bound<'py, PyAny>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = ctx.py;
    let Ok(dt) = value.cast_exact::<PyDateTime>() else {
        return Ok(None);
    };
    let target = match tz {
        DateTimeTz::Fixed(tz) => Some(tz.bind(py).clone()),
        DateTimeTz::Current => ctx.current_tz.clone(),
        DateTimeTz::Naive => None,
    };
    let tzinfo = dt.get_tzinfo();
    // A `datetime.timezone` always has an offset; any other tzinfo may still report a naive value.
    let aware = match &tzinfo {
        None => false,
        Some(info) if is_exact(info, fixed_timezone_type(py)?) => true,
        Some(_) => utcoffset(value)?.is_some(),
    };
    let text = match (target, aware) {
        (None, false) => datetime_iso(dt, None),
        (Some(target), true) => {
            let converted = match &tzinfo {
                Some(info) if info.is(&target) => value.clone(),
                _ => value.call_method1(intern!(py, "astimezone"), (target,))?,
            };
            let Ok(converted_dt) = converted.cast_exact::<PyDateTime>() else {
                return Ok(None);
            };
            match utcoffset(&converted)? {
                Some(offset) => datetime_iso(converted_dt, Some(&offset)),
                None => return Ok(None),
            }
        }
        // make_aware / make_naive: leave it to DRF.
        _ => return Ok(None),
    };
    Ok(Some(PyString::new(py, &text).into_any()))
}

fn choice_repr<'py>(
    map: &Bound<'py, PyDict>,
    value: &Bound<'py, PyAny>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let key = if let Ok(s) = value.cast_exact::<PyString>() {
        if s.to_str()?.is_empty() {
            return Ok(Some(value.clone()));
        }
        s.clone()
    } else if value.is_exact_instance_of::<PyInt>() {
        value.str()?
    } else {
        return Ok(None);
    };
    Ok(Some(match map.get_item(key)? {
        Some(mapped) => mapped,
        None => value.clone(),
    }))
}
