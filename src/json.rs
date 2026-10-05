use std::ffi::CStr;

use jiter::{FloatMode, PartialMode, PythonParse, StringCacheMode};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{
    PyBool, PyByteArray, PyBytes, PyDate, PyDateTime, PyDict, PyFloat, PyInt, PyList, PyString,
    PyTuple,
};
use pyo3::{create_exception, ffi, intern};

use crate::format::{date_iso, datetime_iso, utcoffset, uuid_str};
use crate::tools::{decimal_type, is_exact, uuid_type};

create_exception!(
    _drf_oxide_core,
    JsonFallback,
    pyo3::exceptions::PyException,
    "The native encoder cannot reproduce json.dumps() for this input; use the Python encoder."
);

const MAX_DEPTH: usize = 512;

struct Encoder<'a, 'py> {
    out: Vec<u8>,
    ensure_ascii: bool,
    allow_nan: bool,
    item_sep: &'static [u8],
    key_sep: &'static [u8],
    default: Option<&'a Bound<'py, PyAny>>,
}

/// `json.dumps(obj, cls=rest_framework.utils.encoders.JSONEncoder, ...)` followed by DRF's
/// ` ` / ` ` escaping, encoded as UTF-8. `default` is called for objects the encoder
/// does not know natively (like `JSONEncoder.default`).
#[pyfunction]
#[pyo3(signature = (obj, *, ensure_ascii=true, compact=true, allow_nan=false, default=None))]
pub fn to_json<'py>(
    obj: &Bound<'py, PyAny>,
    ensure_ascii: bool,
    compact: bool,
    allow_nan: bool,
    default: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyBytes>> {
    let mut encoder = Encoder {
        out: Vec::with_capacity(4096),
        ensure_ascii,
        allow_nan,
        item_sep: if compact { b"," } else { b", " },
        key_sep: if compact { b":" } else { b": " },
        default: default.as_ref(),
    };
    encoder.encode(obj, 0)?;
    Ok(PyBytes::new(obj.py(), &encoder.out))
}

/// `json.loads(data)`. Raises `ValueError` on invalid input; callers that need the exact message
/// of the `json` module re-parse with it.
#[pyfunction]
#[pyo3(signature = (data, *, allow_nan=false))]
pub fn from_json<'py>(data: &Bound<'py, PyAny>, allow_nan: bool) -> PyResult<Bound<'py, PyAny>> {
    let py = data.py();
    let parser = PythonParse {
        allow_inf_nan: allow_nan,
        cache_mode: StringCacheMode::Keys,
        partial_mode: PartialMode::Off,
        catch_duplicate_keys: false,
        float_mode: FloatMode::Float,
    };
    let owned;
    let bytes: &[u8] = if let Ok(bytes) = data.cast::<PyBytes>() {
        bytes.as_bytes()
    } else if let Ok(s) = data.cast::<PyString>() {
        s.to_str()?.as_bytes()
    } else if let Ok(array) = data.cast::<PyByteArray>() {
        owned = array.to_vec();
        &owned
    } else {
        return Err(PyTypeError::new_err(
            "from_json() expects bytes, bytearray or str",
        ));
    };
    parser
        .python_parse(py, bytes)
        .map_err(|e| PyValueError::new_err(e.description(bytes)))
}

impl<'py> Encoder<'_, 'py> {
    fn encode(&mut self, obj: &Bound<'py, PyAny>, depth: usize) -> PyResult<()> {
        if depth > MAX_DEPTH {
            return Err(JsonFallback::new_err("nesting too deep"));
        }
        let py = obj.py();
        // Same order of checks as the C encoder of the json module.
        if obj.is_none() {
            self.out.extend_from_slice(b"null");
        } else if let Ok(b) = obj.cast_exact::<PyBool>() {
            self.out
                .extend_from_slice(if b.is_true() { b"true" } else { b"false" });
        } else if let Ok(s) = obj.cast::<PyString>() {
            self.string(s)?;
        } else if obj.is_instance_of::<PyInt>() {
            self.int(obj)?;
        } else if let Ok(f) = obj.cast::<PyFloat>() {
            self.float(f.value())?;
        } else if obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>() {
            self.out.push(b'[');
            let mut first = true;
            for item in obj.try_iter()? {
                if !first {
                    self.out.extend_from_slice(self.item_sep);
                }
                first = false;
                self.encode(&item?, depth + 1)?;
            }
            self.out.push(b']');
        } else if let Ok(dict) = obj.cast::<PyDict>() {
            self.dict(dict, depth)?;
        } else {
            self.other(obj, depth, py)?;
        }
        Ok(())
    }

    fn dict(&mut self, dict: &Bound<'py, PyDict>, depth: usize) -> PyResult<()> {
        self.out.push(b'{');
        let mut first = true;
        let mut item =
            |enc: &mut Self, key: Bound<'py, PyAny>, value: Bound<'py, PyAny>| -> PyResult<()> {
                if !first {
                    enc.out.extend_from_slice(enc.item_sep);
                }
                first = false;
                enc.key(&key)?;
                enc.out.extend_from_slice(enc.key_sep);
                enc.encode(&value, depth + 1)
            };
        if dict.is_exact_instance_of::<PyDict>() {
            for (key, value) in dict.iter() {
                item(self, key, value)?;
            }
        } else {
            // OrderedDict & co. keep their own order, honour `items()` like the C encoder.
            for pair in dict.call_method0(intern!(dict.py(), "items"))?.try_iter()? {
                let (key, value) = pair?.extract()?;
                item(self, key, value)?;
            }
        }
        self.out.push(b'}');
        Ok(())
    }

    fn key(&mut self, key: &Bound<'py, PyAny>) -> PyResult<()> {
        if let Ok(s) = key.cast::<PyString>() {
            return self.string(s);
        }
        let start = self.out.len();
        if key.is_none() {
            self.out.extend_from_slice(b"null");
        } else if let Ok(b) = key.cast_exact::<PyBool>() {
            self.out
                .extend_from_slice(if b.is_true() { b"true" } else { b"false" });
        } else if key.is_instance_of::<PyInt>() {
            self.int(key)?;
        } else if let Ok(f) = key.cast::<PyFloat>() {
            self.float(f.value())?;
        } else {
            return Err(PyTypeError::new_err(format!(
                "keys must be str, int, float, bool or None, not {}",
                key.get_type().name()?
            )));
        }
        // Non-string keys are written as strings.
        self.out.insert(start, b'"');
        self.out.push(b'"');
        Ok(())
    }

    fn int(&mut self, obj: &Bound<'py, PyAny>) -> PyResult<()> {
        match obj.extract::<i64>() {
            Ok(v) => self
                .out
                .extend_from_slice(itoa::Buffer::new().format(v).as_bytes()),
            Err(_) => {
                let py = obj.py();
                let repr = py
                    .get_type::<PyInt>()
                    .call_method1(intern!(py, "__repr__"), (obj,))?;
                self.out
                    .extend_from_slice(repr.cast::<PyString>()?.to_str()?.as_bytes());
            }
        }
        Ok(())
    }

    fn float(&mut self, v: f64) -> PyResult<()> {
        if !v.is_finite() {
            if !self.allow_nan {
                return Err(PyValueError::new_err(format!(
                    "Out of range float values are not JSON compliant: {}",
                    if v.is_nan() {
                        "nan"
                    } else if v > 0.0 {
                        "inf"
                    } else {
                        "-inf"
                    }
                )));
            }
            self.out.extend_from_slice(if v.is_nan() {
                b"NaN".as_slice()
            } else if v > 0.0 {
                b"Infinity".as_slice()
            } else {
                b"-Infinity".as_slice()
            });
            return Ok(());
        }
        // Exactly what `float.__repr__` does.
        unsafe {
            let buf = ffi::PyOS_double_to_string(
                v,
                b'r' as _,
                0,
                ffi::Py_DTSF_ADD_DOT_0,
                std::ptr::null_mut(),
            );
            if buf.is_null() {
                return Err(PyErr::fetch(Python::assume_attached()));
            }
            self.out.extend_from_slice(CStr::from_ptr(buf).to_bytes());
            ffi::PyMem_Free(buf.cast());
        }
        Ok(())
    }

    fn string(&mut self, s: &Bound<'py, PyString>) -> PyResult<()> {
        // A lone surrogate cannot be UTF-8 encoded; json + `.encode()` behave differently per mode.
        let text = s
            .to_str()
            .map_err(|_| JsonFallback::new_err("string with surrogates"))?;
        self.out.push(b'"');
        let bytes = text.as_bytes();
        let mut start = 0;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            let plain = if self.ensure_ascii {
                (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\'
            } else {
                // U+2028 / U+2029 are E2 80 A8 / E2 80 A9 in UTF-8.
                b >= 0x20
                    && b != b'"'
                    && b != b'\\'
                    && !(b == 0xe2
                        && bytes.get(i + 1) == Some(&0x80)
                        && matches!(bytes.get(i + 2), Some(0xa8 | 0xa9)))
            };
            if plain {
                i += 1;
                continue;
            }
            self.out.extend_from_slice(&bytes[start..i]);
            let c = text[i..].chars().next().unwrap_or_default();
            self.escape(c);
            i += c.len_utf8();
            start = i;
        }
        self.out.extend_from_slice(&bytes[start..]);
        self.out.push(b'"');
        Ok(())
    }

    /// A string known to need no escaping.
    fn ascii_string(&mut self, s: &str) -> PyResult<()> {
        self.out.push(b'"');
        self.out.extend_from_slice(s.as_bytes());
        self.out.push(b'"');
        Ok(())
    }

    fn escape(&mut self, c: char) {
        match c {
            '"' => self.out.extend_from_slice(b"\\\""),
            '\\' => self.out.extend_from_slice(b"\\\\"),
            '\n' => self.out.extend_from_slice(b"\\n"),
            '\r' => self.out.extend_from_slice(b"\\r"),
            '\t' => self.out.extend_from_slice(b"\\t"),
            '\u{8}' => self.out.extend_from_slice(b"\\b"),
            '\u{c}' => self.out.extend_from_slice(b"\\f"),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    self.out
                        .extend_from_slice(format!("\\u{:04x}", unit).as_bytes());
                }
            }
        }
    }

    /// `JSONEncoder.default()`: the common DRF cases natively, everything else via `default`.
    fn other(&mut self, obj: &Bound<'py, PyAny>, depth: usize, py: Python<'py>) -> PyResult<()> {
        if let Ok(dt) = obj.cast_exact::<PyDateTime>() {
            let offset = utcoffset(obj)?;
            return self.ascii_string(&datetime_iso(dt, offset.as_ref()));
        }
        if let Ok(date) = obj.cast_exact::<PyDate>() {
            return self.ascii_string(&date_iso(date));
        }
        if is_exact(obj, decimal_type(py)?) {
            // DRF encodes decimals the serializer did not coerce to strings as floats.
            let value: f64 = py.get_type::<PyFloat>().call1((obj,))?.extract()?;
            return self.float(value);
        }
        if is_exact(obj, uuid_type(py)?) {
            return self.string(&uuid_str(obj)?);
        }
        match self.default {
            Some(default) => {
                let replacement = default.call1((obj,))?;
                self.encode(&replacement, depth + 1)
            }
            None => Err(PyTypeError::new_err(format!(
                "Object of type {} is not JSON serializable",
                obj.get_type().name()?
            ))),
        }
    }
}
