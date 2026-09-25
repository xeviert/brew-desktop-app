//! Record boundaries in a byte buffer, before any dialect has looked at it.
//!
//! A record ends at a `\n` outside CSV-style quotes (`"…"`, with `""` as the escape).
//! A new dialect must keep to that: JSON's `\"` escape toggles the scan, so a JSON line
//! holding an odd number of escaped quotes would swallow the next record boundary.

/// Every record boundary in `bytes`, as an offset just past a terminator.
///
/// A `\r\n` inside a quoted field is **not** a boundary: Python's writer quotes any
/// field containing `,` `"` `\r` or `\n`, so a multi-line annotation is reachable, and
/// a naive `rfind` for `\r\n` would cut a record in half.
fn boundaries(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
    let mut in_quotes = false;
    bytes.iter().enumerate().filter_map(move |(i, byte)| {
        match byte {
            // An escaped `""` toggles twice and lands back inside the field, which is
            // the correct answer.
            b'"' => {
                in_quotes = !in_quotes;
                None
            }
            // Keyed on `\n`: `\r\n` ends with it, and a lone `\n` from a hand-edited
            // file still terminates a record as far as the csv reader is concerned.
            b'\n' if !in_quotes => Some(i + 1),
            _ => None,
        }
    })
}

/// Split a buffer into `(complete records, trailing partial record)`.
///
/// **Split before you parse.** The `csv` crate treats EOF as a record terminator, so a
/// trailing half-written row parses as a complete one and `bytes_consumed` claims bytes
/// the writer never finished. Both failures are silent and neither is detectable
/// afterwards: a row cut at `,0,C,1` has the right cell count and a valid `log_time`,
/// so it commits as a real reading while the genuine row's tail arrives next poll as a
/// one-cell record and is skipped; and Rust's `f64` parser accepts `18.`, so a value
/// cut mid-number stores 18.0 where the file says 18.06.
///
/// [`ingest`](fn@super::ingest) keeps no remainder buffer — `byte_offset` only ever
/// advances past a complete record, so the tail is re-requested for free by the next
/// `Range: bytes=<offset>-`. This is what makes that safe.
///
/// `bytes` must start on a record boundary — otherwise the quote state is inverted
/// from the first byte and every boundary after it is wrong. That is `ingest`'s
/// invariant to keep, and the reason a resume that cannot prove it refetches from zero.
pub fn split_complete_lines(bytes: &[u8]) -> (&[u8], &[u8]) {
    bytes.split_at(boundaries(bytes).last().unwrap_or(0))
}

/// Split the first complete record off a buffer: `(the record including its
/// terminator, the rest)`, or `None` when no record is complete.
///
/// Quote-aware for the same reason [`split_complete_lines`] is, and subject to the same
/// start-on-a-boundary invariant.
///
/// [`ingest`](fn@super::ingest) uses this twice. Once to lift the header line off a fetch
/// that began at byte 0, so that one [`Dialect::parse`](super::Dialect::parse) handles
/// the first chunk and every continuation alike and `bytes_consumed` means the same
/// thing in both. Once to prove a resume chunk really does begin on a record boundary,
/// by re-parsing its first record alone.
pub fn split_first_record(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    boundaries(bytes).next().map(|at| bytes.split_at(at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_complete_lines_carries_a_partial_row_forward() {
        let (complete, remainder) = split_complete_lines(b"a,b\r\nc,d\r\ne,");
        assert_eq!(complete, b"a,b\r\nc,d\r\n");
        assert_eq!(remainder, b"e,");
    }

    #[test]
    fn split_complete_lines_handles_the_empty_and_whole_cases() {
        assert_eq!(split_complete_lines(b""), (&b""[..], &b""[..]));
        assert_eq!(
            split_complete_lines(b"a,b\r\n"),
            (&b"a,b\r\n"[..], &b""[..])
        );
        // No terminator at all: nothing is complete yet.
        assert_eq!(split_complete_lines(b"a,b"), (&b""[..], &b"a,b"[..]));
    }

    /// The case a naive `rfind` for CRLF gets wrong. Python's writer quotes any field
    /// containing `\r` or `\n`, so a multi-line annotation is reachable — and cutting
    /// a record in half here would advance `byte_offset` past the wreckage.
    #[test]
    fn a_crlf_inside_a_quoted_annotation_is_not_a_record_boundary() {
        let bytes = b"a,\"two\r\nlines\",b\r\nc,d,";
        let (complete, remainder) = split_complete_lines(bytes);
        assert_eq!(complete, b"a,\"two\r\nlines\",b\r\n");
        assert_eq!(remainder, b"c,d,");
    }

    #[test]
    fn split_first_record_stops_at_the_first_unquoted_terminator() {
        let (first, rest) =
            split_first_record(b"a,b\r\nc,d\r\ne,").expect("one record is complete");
        assert_eq!(first, b"a,b\r\n");
        assert_eq!(rest, b"c,d\r\ne,");

        // A quoted terminator is not a boundary here either.
        let (first, rest) = split_first_record(b"a,\"two\r\nlines\",b\r\nc,d\r\n").unwrap();
        assert_eq!(first, b"a,\"two\r\nlines\",b\r\n");
        assert_eq!(rest, b"c,d\r\n");

        // One terminated record leaves nothing behind.
        assert_eq!(
            split_first_record(b"a,b\r\n"),
            Some((&b"a,b\r\n"[..], &b""[..]))
        );

        // Nothing is complete yet.
        assert_eq!(split_first_record(b"a,b"), None);
        assert_eq!(split_first_record(b""), None);
    }

    /// A doubled quote returns the scanner to "inside the field", not outside it.
    #[test]
    fn an_escaped_quote_does_not_confuse_the_boundary_scan() {
        let bytes = b"a,\"he said \"\"hi\"\"\r\nstill inside\",b\r\nnext";
        let (complete, remainder) = split_complete_lines(bytes);
        assert_eq!(complete, b"a,\"he said \"\"hi\"\"\r\nstill inside\",b\r\n");
        assert_eq!(remainder, b"next");
    }
}
