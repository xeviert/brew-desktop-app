//! Python's `csv.writer` default dialect, byte for byte.
//!
//! Fermentrack writes its logs with a bare `csv.writer(f)` (`app/models.py`,
//! `BeerLogPoint.save`), which is the `excel` dialect: comma delimiter, `"`
//! quotechar, `QUOTE_MINIMAL`, quotes escaped by doubling, and — the part that
//! matters most to us — **`\r\n` as the line terminator**. Byte offsets are the
//! whole sync strategy, so a wrong line ending here would make every resume test
//! agree with a generator that lies.

/// One CSV cell, as Fermentrack would have handed it to `csv.writer`.
#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    Text(String),
    Float(f64),
    Int(i64),
    /// Python `None`, which `csv.writer` writes as an empty, unquoted cell. Only
    /// `beer_ann` and `fridge_ann` are ever `None` in a real log.
    Null,
}

impl Field {
    fn render(&self) -> String {
        match self {
            Field::Text(s) => s.clone(),
            Field::Float(v) => py_float(*v),
            Field::Int(v) => v.to_string(),
            Field::Null => String::new(),
        }
    }
}

/// Append one row, terminated `\r\n`.
pub fn write_row(out: &mut String, fields: &[Field]) {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&quote(&field.render()));
    }
    out.push_str("\r\n");
}

/// `QUOTE_MINIMAL`: quote only when the cell contains the delimiter, the quotechar,
/// or a line terminator character. A leading or trailing space does *not* trigger
/// quoting in Python, and must not here either.
fn quote(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// `str()` of a Python float.
///
/// The values in a real log are JSON numbers assigned straight onto an unmanaged
/// Django model (`app/api/devices.py`, `create_beer_log_point`), so they are never
/// coerced to `Decimal` and reach the CSV as `str(float)`. Rust and Python both
/// print the shortest representation that round-trips, but Rust prints `18` where
/// Python prints `18.0` — one byte, and every offset after it moves.
///
/// Not reproduced: Python switches to exponent form at `1e16` and below `1e-4`.
/// Nothing in a fermentation log lives there, and a fixture that did would be
/// lying about something else already.
pub fn py_float(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    let s = format!("{value}");
    if s.contains(['.', 'e', 'E']) {
        s
    } else {
        format!("{s}.0")
    }
}

/// Snap to `places` decimals the way a rounded JSON number would have arrived.
///
/// Via the formatter rather than `(x * 100.0).round() / 100.0`, which reintroduces
/// the representation error it was meant to remove: `18.06` round-trips through
/// that to `18.060000000000002`, and `py_float` would faithfully print all of it.
pub fn round_to(value: f64, places: usize) -> f64 {
    format!("{value:.places$}").parse().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(fields: &[Field]) -> String {
        let mut out = String::new();
        write_row(&mut out, fields);
        out
    }

    #[test]
    fn rows_end_crlf() {
        assert_eq!(row(&[Field::Int(1)]), "1\r\n");
    }

    /// Verified against CPython:
    /// `[str(x) for x in (18.0, 18.06, 0.1+0.2, 1.043, -0.5, 1e-3)]`.
    #[test]
    fn floats_print_the_way_python_prints_them() {
        assert_eq!(py_float(18.0), "18.0");
        assert_eq!(py_float(-18.0), "-18.0");
        assert_eq!(py_float(0.0), "0.0");
        assert_eq!(py_float(18.06), "18.06");
        assert_eq!(py_float(1.043), "1.043");
        assert_eq!(py_float(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(py_float(0.001), "0.001");
        assert_eq!(py_float(-0.5), "-0.5");
    }

    /// A missing reading is Python's `x or 0` — the integer zero, not `0.0`. That is
    /// the only way the parser can tell an absent sensor from a real reading, so the
    /// two must not render alike.
    #[test]
    fn an_absent_sensor_is_bare_zero_and_a_real_zero_is_not() {
        assert_eq!(row(&[Field::Int(0)]), "0\r\n");
        assert_eq!(row(&[Field::Float(0.0)]), "0.0\r\n");
    }

    #[test]
    fn a_null_cell_is_empty_and_unquoted() {
        assert_eq!(row(&[Field::Null, Field::Int(2)]), ",2\r\n");
    }

    #[test]
    fn quoting_is_minimal() {
        assert_eq!(row(&[Field::Text("plain".into())]), "plain\r\n");
        assert_eq!(row(&[Field::Text(" padded ".into())]), " padded \r\n");
        assert_eq!(row(&[Field::Text("a, b".into())]), "\"a, b\"\r\n");
        assert_eq!(
            row(&[Field::Text("he said \"hi\"".into())]),
            "\"he said \"\"hi\"\"\"\r\n"
        );
        assert_eq!(
            row(&[Field::Text("two\nlines".into())]),
            "\"two\nlines\"\r\n"
        );
    }

    #[test]
    fn rounding_does_not_reintroduce_representation_error() {
        assert_eq!(py_float(round_to(18.058, 2)), "18.06");
        assert_eq!(py_float(round_to(1.0432999, 3)), "1.043");
        assert_eq!(py_float(round_to(18.0, 2)), "18.0");
    }
}
