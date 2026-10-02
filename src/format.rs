//! Native versions of the `isoformat()` / `str()` calls DRF makes for every date, datetime and UUID.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDateAccess, PyDateTime, PyDelta, PyDeltaAccess, PyString, PyTimeAccess};

fn push_2(out: &mut String, v: u32) {
    out.push((b'0' + (v / 10 % 10) as u8) as char);
    out.push((b'0' + (v % 10) as u8) as char);
}

fn push_date(out: &mut String, year: i32, month: u8, day: u8) {
    let year = year as u32;
    push_2(out, year / 100);
    push_2(out, year % 100);
    out.push('-');
    push_2(out, month as u32);
    out.push('-');
    push_2(out, day as u32);
}

/// `date.isoformat()`.
pub fn date_iso(date: &impl PyDateAccess) -> String {
    let mut out = String::with_capacity(10);
    push_date(&mut out, date.get_year(), date.get_month(), date.get_day());
    out
}

/// `datetime.isoformat()` with DRF's `+00:00` -> `Z`. `offset` is `utcoffset()`.
pub fn datetime_iso(dt: &Bound<'_, PyDateTime>, offset: Option<&Bound<'_, PyDelta>>) -> String {
    let mut out = String::with_capacity(32);
    push_date(&mut out, dt.get_year(), dt.get_month(), dt.get_day());
    out.push('T');
    push_2(&mut out, dt.get_hour() as u32);
    out.push(':');
    push_2(&mut out, dt.get_minute() as u32);
    out.push(':');
    push_2(&mut out, dt.get_second() as u32);
    let micro = dt.get_microsecond();
    if micro != 0 {
        out.push_str(&format!(".{micro:06}"));
    }
    if let Some(offset) = offset {
        push_offset(&mut out, offset);
    }
    out
}

fn push_offset(out: &mut String, offset: &Bound<'_, PyDelta>) {
    let total_micros = (offset.get_days() as i64 * 86_400 + offset.get_seconds() as i64)
        * 1_000_000
        + offset.get_microseconds() as i64;
    if total_micros == 0 {
        out.push('Z');
        return;
    }
    out.push(if total_micros < 0 { '-' } else { '+' });
    let abs = total_micros.unsigned_abs();
    let (seconds, micros) = (abs / 1_000_000, abs % 1_000_000);
    push_2(out, (seconds / 3600) as u32);
    out.push(':');
    push_2(out, (seconds / 60 % 60) as u32);
    if seconds % 60 != 0 || micros != 0 {
        out.push(':');
        push_2(out, (seconds % 60) as u32);
        if micros != 0 {
            out.push_str(&format!(".{micros:06}"));
        }
    }
}

/// `str(uuid)`.
pub fn uuid_str<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyString>> {
    let py = value.py();
    let int: u128 = value.getattr(intern!(py, "int"))?.extract()?;
    let hex = format!("{int:032x}");
    Ok(PyString::new(
        py,
        &format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        ),
    ))
}

/// `dt.utcoffset()`; `None` means naive, as in Django's `timezone.is_aware()`.
pub fn utcoffset<'py>(dt: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDelta>>> {
    let offset = dt.call_method0(intern!(dt.py(), "utcoffset"))?;
    if offset.is_none() {
        return Ok(None);
    }
    Ok(Some(offset.cast_into::<PyDelta>()?))
}
