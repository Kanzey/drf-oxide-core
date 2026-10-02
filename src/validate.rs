use pyo3::exceptions::PyValueError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDate, PyDateTime, PyDict, PyFloat, PyInt, PyList, PyString};

use crate::format::utcoffset;
use crate::repr::DateTimeTz;
use crate::serializer::CompiledSerializer;
use crate::tools::{
    decimal_type, opt_bool, opt_item, opt_usize, py_strip, req_item, safe_uuid_unknown, uuid_type,
};

/// Native `to_internal_value` + built-in validators of one field.
///
/// `validate()` returns `None` whenever the input is not the plain, valid case. The caller then
/// runs the whole field through DRF in Python, which produces the exact DRF error (or accepts an
/// input form we do not handle natively). So the native code never has to build an error.
pub enum Val {
    Char {
        allow_blank: bool,
        trim_whitespace: bool,
        max_length: Option<usize>,
        min_length: Option<usize>,
        /// Django's `EmailValidator`; see `is_plain_email`.
        email: bool,
    },
    Int {
        max_value: Option<i64>,
        min_value: Option<i64>,
    },
    Float {
        max_value: Option<f64>,
        min_value: Option<f64>,
    },
    Bool {
        allow_null: bool,
    },
    Decimal {
        max_digits: Option<usize>,
        decimal_places: Option<usize>,
    },
    Date,
    DateTime(DateTimeTz),
    Uuid,
    Choice {
        map: Py<PyDict>,
        allow_blank: bool,
    },
    List {
        child: Box<Val>,
        child_allow_null: bool,
        allow_empty: bool,
        max_length: Option<usize>,
        min_length: Option<usize>,
    },
    /// A nested serializer whose `run_validation` is the stock one.
    Nested(Py<CompiledSerializer>),
    /// A nested `many=True` serializer.
    NestedMany {
        serializer: Py<CompiledSerializer>,
        allow_empty: bool,
        max_length: Option<usize>,
        min_length: Option<usize>,
    },
    Python,
}

const MAX_STRING_LENGTH: usize = 1000;

impl Val {
    pub fn parse(d: &Bound<'_, PyDict>) -> PyResult<Self> {
        let kind: String = req_item(d, "type")?.extract()?;
        Ok(match kind.as_str() {
            "char" => Val::Char {
                allow_blank: opt_bool(d, "allow_blank", false)?,
                trim_whitespace: opt_bool(d, "trim_whitespace", true)?,
                max_length: opt_usize(d, "max_length")?,
                min_length: opt_usize(d, "min_length")?,
                email: opt_bool(d, "email", false)?,
            },
            "int" => Val::Int {
                max_value: opt_item(d, "max_value")?.map(|v| v.extract()).transpose()?,
                min_value: opt_item(d, "min_value")?.map(|v| v.extract()).transpose()?,
            },
            "float" => Val::Float {
                max_value: opt_item(d, "max_value")?.map(|v| v.extract()).transpose()?,
                min_value: opt_item(d, "min_value")?.map(|v| v.extract()).transpose()?,
            },
            "bool" => Val::Bool {
                allow_null: opt_bool(d, "allow_null", false)?,
            },
            "decimal" => Val::Decimal {
                max_digits: opt_usize(d, "max_digits")?,
                decimal_places: opt_usize(d, "decimal_places")?,
            },
            "date" => Val::Date,
            "datetime" => Val::DateTime(DateTimeTz::parse(d)?),
            "uuid" => Val::Uuid,
            "choice" => Val::Choice {
                map: req_item(d, "map")?.cast_into::<PyDict>()?.unbind(),
                allow_blank: opt_bool(d, "allow_blank", false)?,
            },
            "list" => Val::List {
                child: Box::new(Val::parse(req_item(d, "child")?.cast::<PyDict>()?)?),
                child_allow_null: opt_bool(d, "child_allow_null", false)?,
                allow_empty: opt_bool(d, "allow_empty", true)?,
                max_length: opt_usize(d, "max_length")?,
                min_length: opt_usize(d, "min_length")?,
            },
            "nested" => {
                let serializer = req_item(d, "serializer")?
                    .cast_into::<CompiledSerializer>()?
                    .unbind();
                if opt_bool(d, "many", false)? {
                    Val::NestedMany {
                        serializer,
                        allow_empty: opt_bool(d, "allow_empty", true)?,
                        max_length: opt_usize(d, "max_length")?,
                        min_length: opt_usize(d, "min_length")?,
                    }
                } else {
                    Val::Nested(serializer)
                }
            }
            "python" => Val::Python,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown validator type '{other}'"
                )))
            }
        })
    }

    pub fn is_python(&self) -> bool {
        matches!(self, Val::Python)
    }

    pub fn kind(&self) -> String {
        match self {
            Val::Char { .. } => "char".into(),
            Val::Int { .. } => "int".into(),
            Val::Float { .. } => "float".into(),
            Val::Bool { .. } => "bool".into(),
            Val::Decimal { .. } => "decimal".into(),
            Val::Date => "date".into(),
            Val::DateTime(_) => "datetime".into(),
            Val::Uuid => "uuid".into(),
            Val::Choice { .. } => "choice".into(),
            Val::List { child, .. } => format!("list[{}]", child.kind()),
            Val::Nested(_) => "nested".into(),
            Val::NestedMany { .. } => "nested_many".into(),
            Val::Python => "python".into(),
        }
    }

    /// `field.run_validation(data)` minus `validate_empty_values` (the caller handles `empty` and
    /// `None`). `Ok(None)`: not handled natively, use Python.
    pub fn validate<'py>(
        &self,
        data: &Bound<'py, PyAny>,
        current_tz: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let py = data.py();
        match self {
            Val::Char {
                allow_blank,
                trim_whitespace,
                max_length,
                min_length,
                email,
            } => {
                let Ok(s) = data.cast_exact::<PyString>() else {
                    return Ok(None);
                };
                // Lone surrogates fail here; ProhibitSurrogateCharactersValidator rejects them in Python.
                let Ok(text) = s.to_str() else {
                    return Ok(None);
                };
                let value = if *trim_whitespace {
                    py_strip(text)
                } else {
                    text
                };
                if text.is_empty() || value.is_empty() {
                    return Ok(if *allow_blank {
                        Some(PyString::new(py, "").into_any())
                    } else {
                        None
                    });
                }
                if value.contains('\0') || (*email && !is_plain_email(value)) {
                    return Ok(None);
                }
                if max_length.is_some() || min_length.is_some() {
                    let len = value.chars().count();
                    if max_length.is_some_and(|max| len > max)
                        || min_length.is_some_and(|min| len < min)
                    {
                        return Ok(None);
                    }
                }
                if value.len() == text.len() {
                    Ok(Some(data.clone()))
                } else {
                    Ok(Some(PyString::new(py, value).into_any()))
                }
            }
            Val::Int {
                max_value,
                min_value,
            } => {
                let parsed: Option<i64> = if data.is_exact_instance_of::<PyInt>() {
                    data.extract().ok()
                } else if let Ok(s) = data.cast_exact::<PyString>() {
                    s.to_str().ok().and_then(parse_int_str)
                } else if let Ok(f) = data.cast_exact::<PyFloat>() {
                    let v = f.value();
                    (v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15).then_some(v as i64)
                } else {
                    None
                };
                let Some(value) = parsed else {
                    return Ok(None);
                };
                if max_value.is_some_and(|max| value > max)
                    || min_value.is_some_and(|min| value < min)
                {
                    return Ok(None);
                }
                if data.is_exact_instance_of::<PyInt>() {
                    Ok(Some(data.clone()))
                } else {
                    Ok(Some(value.into_pyobject(py)?.into_any()))
                }
            }
            Val::Float {
                max_value,
                min_value,
            } => {
                let value: f64 = if let Ok(f) = data.cast_exact::<PyFloat>() {
                    f.value()
                } else if data.is_exact_instance_of::<PyInt>() {
                    match data.extract::<i64>() {
                        Ok(v) => v as f64,
                        Err(_) => return Ok(None),
                    }
                } else {
                    return Ok(None);
                };
                if !value.is_finite()
                    || max_value.is_some_and(|max| value > max)
                    || min_value.is_some_and(|min| value < min)
                {
                    return Ok(None);
                }
                Ok(Some(PyFloat::new(py, value).into_any()))
            }
            Val::Bool { allow_null } => {
                if data.is_exact_instance_of::<PyBool>() {
                    return Ok(Some(data.clone()));
                }
                let result = if let Ok(s) = data.cast_exact::<PyString>() {
                    match s.to_str().unwrap_or("") {
                        "t" | "T" | "y" | "Y" | "yes" | "Yes" | "YES" | "true" | "True"
                        | "TRUE" | "on" | "On" | "ON" | "1" => Some(true),
                        "f" | "F" | "n" | "N" | "no" | "No" | "NO" | "false" | "False"
                        | "FALSE" | "off" | "Off" | "OFF" | "0" => Some(false),
                        "null" | "Null" | "NULL" | "" if *allow_null => {
                            return Ok(Some(py.None().into_bound(py)));
                        }
                        _ => None,
                    }
                } else if data.is_exact_instance_of::<PyInt>() {
                    match data.extract::<i64>() {
                        Ok(1) => Some(true),
                        Ok(0) => Some(false),
                        _ => None,
                    }
                } else {
                    None
                };
                Ok(result.map(|b| PyBool::new(py, b).to_owned().into_any()))
            }
            Val::Decimal {
                max_digits,
                decimal_places,
            } => {
                let owned;
                let text: &str = if let Ok(s) = data.cast_exact::<PyString>() {
                    match s.to_str() {
                        Ok(t) => t,
                        Err(_) => return Ok(None),
                    }
                } else if data.is_exact_instance_of::<PyInt>() {
                    match data.extract::<i64>() {
                        Ok(v) => {
                            owned = v.to_string();
                            &owned
                        }
                        Err(_) => return Ok(None),
                    }
                } else {
                    return Ok(None);
                };
                match normalize_decimal(text, *max_digits, *decimal_places) {
                    Some(normalized) => Ok(Some(decimal_type(py)?.call1((normalized,))?)),
                    None => Ok(None),
                }
            }
            Val::Date => {
                if data.is_exact_instance_of::<PyDate>() {
                    return Ok(Some(data.clone()));
                }
                let Ok(s) = data.cast_exact::<PyString>() else {
                    return Ok(None);
                };
                let Some((y, m, d)) = s.to_str().ok().and_then(parse_iso_date) else {
                    return Ok(None);
                };
                Ok(PyDate::new(py, y, m, d).ok().map(Bound::into_any))
            }
            Val::DateTime(tz) => {
                let Ok(s) = data.cast_exact::<PyString>() else {
                    return Ok(None);
                };
                let target = match tz {
                    DateTimeTz::Fixed(tz) => tz.bind(py).clone(),
                    DateTimeTz::Current => match current_tz {
                        Some(tz) => tz.clone(),
                        None => return Ok(None),
                    },
                    DateTimeTz::Naive => return Ok(None),
                };
                // Django's parse_datetime() tries fromisoformat() first; only aware results are
                // handled here, naive ones need make_aware().
                let Ok(parsed) = py
                    .get_type::<PyDateTime>()
                    .call_method1(intern!(py, "fromisoformat"), (s,))
                else {
                    return Ok(None);
                };
                if utcoffset(&parsed)?.is_none() {
                    return Ok(None);
                }
                Ok(parsed
                    .call_method1(intern!(py, "astimezone"), (target,))
                    .ok())
            }
            Val::Uuid => {
                let Ok(s) = data.cast_exact::<PyString>() else {
                    return Ok(None);
                };
                match s.to_str().ok().and_then(parse_uuid) {
                    Some(value) => Ok(Some(new_uuid(py, value)?)),
                    None => Ok(None),
                }
            }
            Val::Choice { map, allow_blank } => {
                let key = if let Ok(s) = data.cast_exact::<PyString>() {
                    if *allow_blank && s.to_str().is_ok_and(str::is_empty) {
                        return Ok(Some(data.clone()));
                    }
                    s.clone()
                } else if data.is_exact_instance_of::<PyInt>() {
                    data.str()?
                } else {
                    return Ok(None);
                };
                Ok(map.bind(py).get_item(key)?)
            }
            Val::List {
                child,
                child_allow_null,
                allow_empty,
                max_length,
                min_length,
            } => {
                let Ok(list) = data.cast_exact::<PyList>() else {
                    return Ok(None);
                };
                let len = list.len();
                if (!allow_empty && len == 0)
                    || max_length.is_some_and(|max| len > max)
                    || min_length.is_some_and(|min| len < min)
                {
                    return Ok(None);
                }
                let out = PyList::empty(py);
                for item in list.iter() {
                    if item.is_none() {
                        if !child_allow_null {
                            return Ok(None);
                        }
                        out.append(item)?;
                        continue;
                    }
                    match child.validate(&item, current_tz)? {
                        Some(value) => out.append(value)?,
                        None => return Ok(None),
                    }
                }
                Ok(Some(out.into_any()))
            }
            Val::Nested(serializer) => {
                let Ok(dict) = data.cast_exact::<PyDict>() else {
                    return Ok(None);
                };
                // Errors are rebuilt by DRF, which then also formats them.
                Ok(serializer.get().validate_dict(dict, current_tz)?.ok())
            }
            Val::NestedMany {
                serializer,
                allow_empty,
                max_length,
                min_length,
            } => {
                let Ok(list) = data.cast_exact::<PyList>() else {
                    return Ok(None);
                };
                let len = list.len();
                if (!allow_empty && len == 0)
                    || max_length.is_some_and(|max| len > max)
                    || min_length.is_some_and(|min| len < min)
                {
                    return Ok(None);
                }
                let serializer = serializer.get();
                let out = PyList::empty(py);
                for item in list.iter() {
                    let Ok(dict) = item.cast_exact::<PyDict>() else {
                        return Ok(None);
                    };
                    match serializer.validate_dict(dict, current_tz)? {
                        Ok(value) => out.append(value)?,
                        Err(_) => return Ok(None),
                    }
                }
                Ok(Some(out.into_any()))
            }
            Val::Python => Ok(None),
        }
    }
}

/// A conservative subset of what Django's `EmailValidator` accepts, identical across Django
/// versions: an ASCII dot-atom local part and a domain of LDH labels ending in an alphabetic TLD.
/// Anything outside it (quoted local parts, IDNs, IP literals, `localhost`) is checked by Django.
pub fn is_plain_email(value: &str) -> bool {
    if value.len() > 254 || !value.is_ascii() {
        return false;
    }
    let Some((local, domain)) = value.rsplit_once('@') else {
        return false;
    };
    const LOCAL_SPECIALS: &[u8] = b"!#$%&'*+/=?^_`{|}~-";
    let local_ok = !local.is_empty()
        && local.split('.').all(|atom| {
            !atom.is_empty()
                && atom
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || LOCAL_SPECIALS.contains(&b))
        });
    if !local_ok {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let (tld, hosts) = labels.split_last().unwrap();
    let tld_ok = (2..=63).contains(&tld.len()) && tld.bytes().all(|b| b.is_ascii_alphabetic());
    tld_ok
        && hosts.iter().all(|label| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

/// `int(re.sub(r'\.0*\s*$', '', s))` for plain ASCII integers that fit in an i64.
fn parse_int_str(s: &str) -> Option<i64> {
    let s = match s.find('.') {
        Some(dot) if s[dot + 1..].bytes().all(|b| b == b'0') => &s[..dot],
        Some(_) => return None,
        None => s,
    };
    let digits = s.strip_prefix(['-', '+']).unwrap_or(s);
    if digits.is_empty() || digits.len() > 18 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The canonical 36-character form or 32 hex digits; everything else goes through `uuid.UUID`.
fn parse_uuid(s: &str) -> Option<u128> {
    let hex: String = match s.len() {
        32 => s.to_owned(),
        36 => {
            let b = s.as_bytes();
            if b[8] != b'-' || b[13] != b'-' || b[18] != b'-' || b[23] != b'-' {
                return None;
            }
            s.split('-').collect()
        }
        _ => return None,
    };
    if hex.len() != 32 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}

/// `uuid.UUID(int=value)` without running `UUID.__init__` (the same state `UUID.__setstate__` sets).
fn new_uuid(py: Python<'_>, value: u128) -> PyResult<Bound<'_, PyAny>> {
    let cls = uuid_type(py)?;
    let obj = cls.call_method1(intern!(py, "__new__"), (cls,))?;
    let set = py.get_type::<PyAny>().getattr(intern!(py, "__setattr__"))?;
    set.call1((&obj, intern!(py, "int"), value))?;
    set.call1((&obj, intern!(py, "is_safe"), safe_uuid_unknown(py)?))?;
    Ok(obj)
}

/// Strict `YYYY-MM-DD`.
fn parse_iso_date(s: &str) -> Option<(i32, u8, u8)> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = &s[r];
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    Some((num(0..4)? as i32, num(5..7)? as u8, num(8..10)? as u8))
}

/// `DecimalField.to_internal_value` for plain `[+-]digits[.digits]` input: runs
/// `validate_precision` and returns the string of the quantized value, or `None` when DRF would
/// either reject the input or has to round it.
pub fn normalize_decimal(
    text: &str,
    max_digits: Option<usize>,
    decimal_places: Option<usize>,
) -> Option<String> {
    let s = text.trim_ascii();
    if s.is_empty() || s.len() > MAX_STRING_LENGTH {
        return None;
    }
    let (negative, unsigned) = match s.as_bytes()[0] {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let (int_part, frac_part) = match unsigned.split_once('.') {
        Some((i, f)) => (i, f),
        None => (unsigned, ""),
    };
    if (int_part.is_empty() && frac_part.is_empty())
        || !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }

    // Same digit counting as `Decimal.as_tuple()` in `validate_precision`.
    let coefficient_digits = {
        let joined_len = int_part.len() + frac_part.len();
        let leading_zeros = int_part
            .bytes()
            .chain(frac_part.bytes())
            .take_while(|b| *b == b'0')
            .count();
        (joined_len - leading_zeros).max(1)
    };
    let exponent = frac_part.len();
    let (total_digits, whole_digits, places) = if exponent == 0 {
        (coefficient_digits, coefficient_digits, 0)
    } else if coefficient_digits > exponent {
        (coefficient_digits, coefficient_digits - exponent, exponent)
    } else {
        (exponent, 0, exponent)
    };
    if max_digits.is_some_and(|max| total_digits > max)
        || decimal_places.is_some_and(|dp| places > dp)
    {
        return None;
    }
    if let (Some(max), Some(dp)) = (max_digits, decimal_places) {
        if whole_digits > max.saturating_sub(dp) {
            return None;
        }
    }

    let mut out = String::with_capacity(s.len() + decimal_places.unwrap_or(0));
    if negative {
        out.push('-');
    }
    match decimal_places {
        None => {
            out.push_str(if int_part.is_empty() { "0" } else { int_part });
            if !frac_part.is_empty() {
                out.push('.');
                out.push_str(frac_part);
            }
        }
        Some(dp) => {
            out.push_str(if int_part.is_empty() { "0" } else { int_part });
            if dp > 0 {
                out.push('.');
                out.push_str(frac_part);
                out.extend(std::iter::repeat_n('0', dp - frac_part.len()));
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_normalization() {
        assert_eq!(
            normalize_decimal("1.5", Some(5), Some(2)).as_deref(),
            Some("1.50")
        );
        assert_eq!(
            normalize_decimal(" -0 ", Some(5), Some(2)).as_deref(),
            Some("-0.00")
        );
        assert_eq!(
            normalize_decimal(".5", Some(5), Some(2)).as_deref(),
            Some("0.50")
        );
        assert_eq!(
            normalize_decimal("1.", Some(5), Some(2)).as_deref(),
            Some("1.00")
        );
        assert_eq!(
            normalize_decimal("12", Some(5), Some(0)).as_deref(),
            Some("12")
        );
        assert_eq!(normalize_decimal("1.555", Some(5), Some(2)), None);
        assert_eq!(normalize_decimal("1234.5", Some(5), Some(2)), None);
        assert_eq!(normalize_decimal("0.001", Some(2), None), None);
        assert_eq!(normalize_decimal("1e5", Some(10), Some(2)), None);
        assert_eq!(normalize_decimal("", Some(10), Some(2)), None);
        assert_eq!(normalize_decimal("-", Some(10), Some(2)), None);
        assert_eq!(
            normalize_decimal("007.5", Some(3), Some(2)).as_deref(),
            Some("007.50")
        );
    }

    #[test]
    fn int_parsing() {
        assert_eq!(parse_int_str("12"), Some(12));
        assert_eq!(parse_int_str("-12.000"), Some(-12));
        assert_eq!(parse_int_str("+7."), Some(7));
        assert_eq!(parse_int_str("1.5"), None);
        assert_eq!(parse_int_str(" 1"), None);
        assert_eq!(parse_int_str(""), None);
    }

    #[test]
    fn plain_emails() {
        for ok in [
            "a@b.pl",
            "first.last+tag@sub.example.com",
            "x_y-z@a-b.co",
            "o'neil@example.org",
        ] {
            assert!(is_plain_email(ok), "{ok}");
        }
        for bad in [
            "a@b",
            "a@localhost",
            ".a@b.pl",
            "a..b@b.pl",
            "a.@b.pl",
            "a@-b.pl",
            "a@b-.pl",
            "a@b.p",
            "a@b.p1",
            "a@b..pl",
            "\"q\"@b.pl",
            "ą@b.pl",
            "a@[127.0.0.1]",
            "a b@b.pl",
            "@b.pl",
            "a@b.pl.",
        ] {
            assert!(!is_plain_email(bad), "{bad}");
        }
    }

    #[test]
    fn uuid_parsing() {
        assert_eq!(
            parse_uuid("12345678-1234-5678-1234-567812345678"),
            Some(0x12345678123456781234567812345678)
        );
        assert_eq!(
            parse_uuid("12345678123456781234567812345678"),
            Some(0x12345678123456781234567812345678)
        );
        assert_eq!(parse_uuid("{12345678-1234-5678-1234-567812345678}"), None);
        assert_eq!(parse_uuid("12345678-1234-5678-1234-56781234567g"), None);
        assert_eq!(parse_uuid("1234567-81234-5678-1234-567812345678"), None);
    }

    #[test]
    fn iso_date() {
        assert_eq!(parse_iso_date("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(parse_iso_date("2024-2-29"), None);
        assert_eq!(parse_iso_date("20240229"), None);
    }
}
