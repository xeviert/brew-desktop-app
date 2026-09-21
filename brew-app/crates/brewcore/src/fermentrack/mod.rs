//! Phase 2. Every Fermentrack HTTP call and CSV parse lives here, so that replacing
//! Fermentrack with our own controller is a new module beside these rather than a
//! rewrite. Fermentrack's `Beer` naming is quarantined to this module.
//!
//! There is deliberately no trait yet. The one the Option 2 door needs is
//! `ReadingSource`, and it belongs at the level of *readings*, not bytes: a v2
//! controller subscribes to MQTT, it does not serve byte ranges of a CSV. Writing it
//! now, around the only implementation that exists, would cement the byte offset into
//! 2g's scheduler — which is the coupling the door is supposed to prevent. It lands in
//! 2g, when there are two callers (the scheduler and the manual Resync command).
//!
//! Nor is there a remainder buffer. [`sync::ingest`] advances `byte_offset` only past
//! complete, terminated records, so the next `Range: bytes=<offset>-` re-requests the
//! partial tail for free — no state to carry across a restart, and no schema column to
//! hold it. The rule that pays for it is **split before you parse**; see
//! [`split_complete_lines`].

pub mod client;
pub mod csv_log;
mod state;
pub mod sync;

pub use client::{Client, ClientSettings, FetchOutcome, RetryPolicy, log_url};
pub use csv_log::{
    LogRow, ParseReport, ParsedChunk, parse_full_log, parse_with_header, split_complete_lines,
    split_first_record,
};
pub use state::ControllerState;
pub use sync::{
    Advance, MAX_PARTIAL_RECORD_BYTES, Restart, RestartReason, SyncFault, SyncState, SyncStep,
    ingest,
};

/// How much of a suspicious body goes in an error message. Enough to recognise an
/// HTML error page, not enough to paste a whole dashboard into the log.
///
/// Shared by the parser and the client on purpose: the day someone tunes this for a
/// misdirected URL, they will not think to look in `csv_log.rs` as well.
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
