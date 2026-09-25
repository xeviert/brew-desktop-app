//! Phase Va. The format-agnostic half of Phase 2: fetch an append-only log by byte
//! range, split it on record boundaries, and commit the offset in the same transaction
//! as the readings it covers.
//!
//! Hoisted out of `fermentrack/` so that `brewd`'s journals reuse it with only the
//! dialect swapped. Nothing here knows what a record *means* — that is a [`Dialect`],
//! and `fermentrack::sync::FullCsv` is the first one. This module never imports
//! `fermentrack`.
//!
//! There is no remainder buffer. [`ingest()`] advances `byte_offset` only past complete,
//! terminated records, so the next `Range: bytes=<offset>-` re-requests the partial
//! tail for free — no state to carry across a restart, and no schema column to hold
//! it. The rule that pays for it is **split before you parse**; see
//! [`split_complete_lines`].

pub mod client;
pub mod ingest;
pub mod lines;
pub mod store;

pub use client::{Client, ClientSettings, FetchOutcome, RetryPolicy, log_url};
pub use ingest::{
    Advance, Dialect, MAX_PARTIAL_RECORD_BYTES, ParseReport, ParsedChunk, Restart, RestartReason,
    SyncFault, SyncState, SyncStep, ingest,
};
pub use lines::{split_complete_lines, split_first_record};
pub use store::{CommitReport, commit, load_sync_state, upsert_sync_state};

/// How much of a suspicious body goes in an error message. Enough to recognise an
/// HTML error page, not enough to paste a whole dashboard into the log.
///
/// Shared by every parser and the client on purpose: the day someone tunes this for a
/// misdirected URL, they will not think to look in each dialect as well.
pub(crate) const DETAIL_CHARS: usize = 200;

/// Truncated on a character boundary, with an ellipsis when anything was cut.
pub(crate) fn preview(text: &str) -> String {
    let truncated: String = text.chars().take(DETAIL_CHARS).collect();
    if truncated.len() < text.len() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// The same, for a body that is not known to be text.
pub(crate) fn preview_bytes(bytes: &[u8]) -> String {
    // Slice before converting: a mis-pointed URL can return megabytes, and allocating
    // all of it lossily only to keep 200 characters is wasteful. Four bytes per
    // character is the UTF-8 maximum, so this can never cut short of `DETAIL_CHARS`;
    // cutting mid-sequence is fine, because lossy conversion emits a replacement
    // character and `preview` then truncates on a real boundary.
    let head = &bytes[..bytes.len().min(DETAIL_CHARS * 4)];
    preview(&String::from_utf8_lossy(head))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mis-pointed URL can return a favicon or a gzip stream. That must produce a
    /// short ugly string, never a panic on a character boundary.
    #[test]
    fn a_preview_of_a_binary_body_does_not_panic_on_a_character_boundary() {
        let body = [0xff_u8, 0xfe, 0x00, 0x80, 0x41];
        let shown = preview_bytes(&body);
        assert!(shown.contains('A'), "{shown}");

        // A multi-byte character straddling the slice point.
        let mut long = "é".repeat(DETAIL_CHARS * 4);
        long.push('!');
        let shown = preview_bytes(long.as_bytes());
        assert!(shown.chars().count() <= DETAIL_CHARS + 1, "{shown}");
    }

    /// The cap is on characters, not bytes, and the ellipsis says it was applied.
    #[test]
    fn a_preview_stops_at_two_hundred_characters() {
        let short = "log_time,beer_temp";
        assert_eq!(preview(short), short);

        let long = "x".repeat(DETAIL_CHARS * 3);
        let shown = preview(&long);
        assert_eq!(shown.chars().count(), DETAIL_CHARS + 1, "{shown}");
        assert!(shown.ends_with('…'), "{shown}");
    }
}
