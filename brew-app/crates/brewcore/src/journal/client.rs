//! Phase 2c, hoisted in Va. Fetching an append-only log incrementally over HTTP —
//! Fermentrack's `_full.csv`, and `brewd`'s journals the same way.
//!
//! This module is pure transport: a URL and a byte offset in, bytes and a resume
//! decision out. It never parses CSV, never touches the database, never reads config.
//! That is what lets Phase 2d be tested against a `Vec<u8>` with no server at all, and
//! what lets Phase 2f reuse `fetch(url, 0)` as its validation probe.
//!
//! Fermentrack's logs are append-only files served by nginx with no auth, so an
//! incremental sync is `Range: bytes=<offset>-` and nothing else — no pagination, no
//! watermark, no dedup pass. Everything subtle here is about the answers that are
//! *not* "here are your new bytes": the file stopped growing, the file was recreated
//! shorter, or the URL is pointing at a dashboard.

use std::cmp::Ordering;
use std::time::Duration;

use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, RANGE};
use reqwest::{StatusCode, Url};

use super::preview_bytes;
use crate::error::{Error, Result};

/// How long to wait for a connection before giving up on a host that is not there.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for the *next chunk* of a response body.
///
/// Deliberately a read timeout rather than `reqwest`'s overall `.timeout()`: "app
/// closed for a week" means one request legitimately carrying tens of megabytes, and a
/// whole-request deadline would cap the backfill. A per-read deadline lets a slow but
/// progressing download run as long as it needs while a half-open connection still
/// dies in 30 seconds.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Content types we will read as CSV. Fermentrack's nginx serves `text/csv`, but a
/// reverse proxy with no mime mapping sends `text/plain` or nothing at all, and both
/// are honest answers. See `check_is_csv` for the backstop.
const ACCEPTABLE_TYPES: [&str; 4] = [
    "text/csv",
    "text/plain",
    "application/csv",
    "application/octet-stream",
];

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

/// What a fetch means for the caller's stored `byte_offset`.
///
/// One variant per distinct persisted-state change Phase 2d has to make. Anything that
/// leaves persisted state alone except `last_error` is an `Err` instead — which is why
/// a 404 is an error and "the file did not grow" is not. A missing log, a timeout, an
/// HTML body and a 500 are all handled identically by the sync loop (record it, keep
/// the source, keep its readings), so giving any of them its own outcome would
/// duplicate that at every call site and invite a `_ => {}` arm that drops it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// New bytes, beginning at exactly `offset`. The 200-vs-206 fix-up has already
    /// been applied, so the caller cannot tell which status it got, and must not care.
    Fresh {
        bytes: Vec<u8>,
        offset: u64,
        total: Option<u64>,
    },
    /// The remote file has not grown. This is what every idle poll returns once the
    /// backfill is done, so it must stay a no-op: treating it as truncation would
    /// refetch the whole log once a minute, forever.
    Unchanged { total: u64 },
    /// The remote file is shorter than `offset`: rotated, recreated, or rebuilt. Reset
    /// `byte_offset` to 0 and refetch whole.
    Truncated { total: Option<u64> },
}

// ---------------------------------------------------------------------------
// The pure layer: headers in, a decision out
// ---------------------------------------------------------------------------

/// A parsed `Content-Range`, in both of the forms a server may send it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentRange {
    /// `bytes <start>-<end>/<total>`, with `total` absent for the `/*` form.
    Satisfied {
        start: u64,
        end: u64,
        total: Option<u64>,
    },
    /// `bytes */<total>` — the 416 form, and the only thing that tells us how long the
    /// file actually is once our offset has run past the end of it.
    Unsatisfied { total: u64 },
}

/// What to do with a response, before any bytes are moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// Keep `body[skip..]`.
    Body {
        skip: usize,
        total: Option<u64>,
    },
    Unchanged {
        total: u64,
    },
    Truncated {
        total: Option<u64>,
    },
}

pub(crate) fn parse_content_range(value: &str) -> Option<ContentRange> {
    let (unit, rest) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }

    // `rsplit_once`, so a malformed range containing a second `/` still loses only the
    // total rather than silently parsing the wrong number.
    let (range, total) = rest.trim().rsplit_once('/')?;
    let total = match total.trim() {
        "*" => None,
        digits => Some(digits.parse::<u64>().ok()?),
    };

    if range.trim() == "*" {
        // `bytes */*` carries no information at all; refuse it rather than inventing one.
        return total.map(|total| ContentRange::Unsatisfied { total });
    }

    let (start, end) = range.trim().split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end: u64 = end.trim().parse().ok()?;
    if end < start {
        return None;
    }
    if matches!(total, Some(total) if end >= total) {
        return None;
    }
    Some(ContentRange::Satisfied { start, end, total })
}

/// Turn a response's status line and `Content-Range` into a decision.
///
/// Kept separate from `fetch` and free of `reqwest` types so that every rule below is
/// testable without a server — including the two answers the fixture harness cannot
/// produce (a 416 with no `Content-Range`, and a 206 whose total is below our offset).
///
/// The caller has already dealt with 404 and with every non-2xx that is not 416, and
/// has already refused a body that is not CSV. That ordering matters: see `fetch`.
pub(crate) fn classify(
    url: &str,
    status: u16,
    content_range: Option<&str>,
    offset: u64,
    body: &[u8],
) -> Result<Disposition> {
    let body_len = body.len() as u64;
    let mismatch = |detail: String| Error::RangeMismatch {
        url: url.to_owned(),
        status,
        detail,
    };

    match status {
        416 => match content_range.and_then(parse_content_range) {
            Some(ContentRange::Unsatisfied { total }) => match total.cmp(&offset) {
                // The steady state of every idle poll: the file is exactly as long as
                // the last sync left it, so there is nothing to fetch.
                Ordering::Equal => Ok(Disposition::Unchanged { total }),
                Ordering::Less => Ok(Disposition::Truncated { total: Some(total) }),
                // The file is longer than our offset, yet the range was refused. We
                // cannot reset — that would ask for byte 0 and get the same answer
                // forever. Wedge visibly instead.
                Ordering::Greater => Err(mismatch(format!(
                    "the file is {total} bytes, past our offset of {offset}, \
                     yet the range was refused"
                ))),
            },
            // No `Content-Range`, or one we cannot read: there is no length to compare
            // against, and the server has told us our offset is past the end. The only
            // safe reading is that the file no longer holds it.
            _ => Ok(Disposition::Truncated { total: None }),
        },

        206 => match content_range.and_then(parse_content_range) {
            Some(ContentRange::Satisfied { start, total, .. }) => {
                if matches!(total, Some(total) if total < offset) {
                    // The same truncation as the 416 case, seen from the other side.
                    return Ok(Disposition::Truncated { total });
                }
                if start != offset {
                    // Trusting this would write rows as though they came from an offset
                    // they did not — the one corruption Phase 2d can never detect.
                    return Err(mismatch(format!(
                        "asked for bytes {offset}- and got a body starting at {start}"
                    )));
                }
                Ok(Disposition::Body { skip: 0, total })
            }
            Some(ContentRange::Unsatisfied { total }) => Err(mismatch(format!(
                "a partial response cannot carry an unsatisfied range (bytes */{total})"
            ))),
            None => {
                tracing::warn!(
                    url,
                    "206 without a readable Content-Range; trusting it starts at our offset"
                );
                Ok(Disposition::Body {
                    skip: 0,
                    total: None,
                })
            }
        },

        200 => match body_len.cmp(&offset) {
            // A Range-ignoring proxy in front of a rotated log. More likely in the wild
            // than the 206 form of the same thing, and the spec is silent on it.
            Ordering::Less => Ok(Disposition::Truncated {
                total: Some(body_len),
            }),
            Ordering::Equal => Ok(Disposition::Unchanged { total: body_len }),
            // The server ignored `Range`; skip the bytes we already have, locally.
            Ordering::Greater => Ok(Disposition::Body {
                // `offset < body_len`, and `body_len` came from a slice length.
                skip: offset as usize,
                total: Some(body_len),
            }),
        },

        other => Err(Error::HttpStatus {
            url: url.to_owned(),
            status: other,
            preview: preview_bytes(body),
        }),
    }
}

/// Reject a body that is not CSV, by declared type and then by shape.
///
/// This belongs in the client rather than the parser because a resume calls
/// `parse_with_header`, which never reads a header: hand it an HTML dashboard and it
/// returns `Ok` with zero rows and a clean `ParseReport`, and 2d then advances
/// `byte_offset` past the whole page. A status check alone cannot catch it either —
/// a misdirected URL answers **200**.
fn check_is_csv(url: &str, content_type: &str, body: &[u8]) -> Result<()> {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let type_is_plausible = essence.is_empty() || ACCEPTABLE_TYPES.contains(&essence.as_str());

    // Backstop for a server that mislabels the body. Safe on a resume as well as a
    // first fetch: a resume body starts on a record boundary, and every Fermentrack
    // record starts with `log_time`, which is a digit. A `<` at byte 0 is never real.
    let looks_like_markup = body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'<');

    if type_is_plausible && !looks_like_markup {
        return Ok(());
    }
    Err(Error::NotCsv {
        url: url.to_owned(),
        content_type: if essence.is_empty() {
            "no content type".to_owned()
        } else {
            essence
        },
        preview: preview_bytes(body),
    })
}

// ---------------------------------------------------------------------------
// URLs
// ---------------------------------------------------------------------------

/// Build the absolute URL of a log from a configured base and a stored path.
///
/// `remote_path` is stored **decoded** — that is how Fermentrack names the file and how
/// the UI shows it — and encoded here, once. Storing it encoded would put two spellings
/// of the same file in `sync_state` and print `%20` at the user.
pub fn log_url(base_url: &str, remote_path: &str) -> Result<String> {
    let base = base_url.trim();
    let path = remote_path.trim();
    let invalid = |url: &str, reason: &str| Error::InvalidUrl {
        url: url.to_owned(),
        reason: reason.to_owned(),
    };

    if path.contains(['\r', '\n', '\0']) {
        return Err(invalid(path, "it contains a control character"));
    }

    // Phase 2f lets the user paste a whole URL, which their browser already encoded.
    // Re-encoding it would turn `%20` into `%2520`.
    let absolute = Url::parse(path)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"));
    if let Some(url) = absolute {
        return Ok(url.into());
    }

    if base.is_empty() {
        return Err(invalid(path, "no base URL is configured"));
    }
    if path.split('/').any(|segment| segment == "..") {
        return Err(invalid(path, "it contains a `..` segment"));
    }

    let mut url = Url::parse(base).map_err(|err| invalid(base, &err.to_string()))?;
    // `set_path`, never `Url::join`: `join` reads a value starting `//` as
    // protocol-relative and would point the fetch at another host entirely, which
    // matters because `remote_path` is pasted by hand in 2f.
    let base_path = url.path().trim_end_matches('/').to_owned();
    let relative = path.trim_start_matches('/');
    url.set_path(&format!("{base_path}/{relative}"));
    Ok(url.into())
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

/// Retry *within* one fetch: a dropped connection, a TLS handshake that lost a race, a
/// 502 from a proxy restarting. Measured in milliseconds.
///
/// Not to be confused with Phase 2g's per-source backoff, which is measured in minutes
/// and stretches the poll interval when a Fermentrack has been off all weekend. Both
/// exist; they cover different failures at different timescales, and neither
/// substitutes for the other.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total attempts, including the first.
    pub attempts: u32,
    pub initial_backoff: Duration,
    pub backoff_factor: u32,
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            initial_backoff: Duration::from_millis(250),
            backoff_factor: 4,
            max_backoff: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClientSettings {
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub retry: RetryPolicy,
    pub user_agent: String,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            retry: RetryPolicy::default(),
            user_agent: concat!("brew-app/", env!("CARGO_PKG_VERSION")).to_owned(),
        }
    }
}

/// One per application, shared. A fresh `reqwest::Client` per poll throws away the
/// connection pool and, over a week of sixty-second ticks, leaves sockets in
/// `TIME_WAIT`.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    retry: RetryPolicy,
}

impl Client {
    pub fn new() -> Result<Self> {
        Self::with_settings(ClientSettings::default())
    }

    pub fn with_settings(settings: ClientSettings) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(settings.connect_timeout)
            .read_timeout(settings.read_timeout)
            .user_agent(settings.user_agent)
            .build()
            .map_err(|source| Error::Http {
                // No URL is in play yet; `build` only fails setting up TLS.
                url: "<client initialisation>".to_owned(),
                source,
            })?;
        Ok(Self {
            http,
            retry: settings.retry,
        })
    }

    /// Fetch everything the log has gained since `offset`.
    pub async fn fetch(&self, url: &str, offset: u64) -> Result<FetchOutcome> {
        self.retrying(url, || self.attempt(url, offset)).await
    }

    /// The first `len` bytes of a log, as **bytes** rather than a [`FetchOutcome`].
    ///
    /// Phase 2f's validation probe. Returning an outcome here would be a trap: a capped
    /// body is `Fresh { offset: 0 }`, indistinguishable from a genuine complete first
    /// fetch, and a caller that passed it to `ingest` would store a truncated log and
    /// advance the offset past bytes it never saw.
    ///
    /// An empty result is not an error — a just-created log really is zero bytes, and
    /// naming that is the validator's job, not the transport's.
    pub async fn fetch_prefix(&self, url: &str, len: u64) -> Result<Vec<u8>> {
        let (bytes, content_type) = self
            .retrying(url, || self.body_attempt(url, Some(len)))
            .await?;
        // Same reason as `attempt`: the parser cannot tell an HTML dashboard from a log
        // it has no header for, and this is the only layer that sees `Content-Type`.
        check_is_csv(url, &content_type, &bytes)?;
        Ok(bytes)
    }

    /// A capped GET with no CSV check, for the one endpoint that is not a log:
    /// `/api/devices/`, which answers JSON.
    ///
    /// `max` caps what is kept, not what is asked for when `None`.
    pub async fn fetch_body(&self, url: &str, max: Option<u64>) -> Result<Vec<u8>> {
        self.retrying(url, || self.body_attempt(url, max))
            .await
            .map(|(bytes, _)| bytes)
    }

    /// Run `f` under the retry policy. Shared by `fetch` and the capped reads so that a
    /// proxy restarting costs one extra attempt in every path, not just the sync one.
    async fn retrying<T, F, Fut>(&self, url: &str, f: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut attempt = 1;
        let mut backoff = self.retry.initial_backoff;
        loop {
            let err = match f().await {
                Ok(value) => return Ok(value),
                Err(err) => err,
            };
            if attempt >= self.retry.attempts || !is_retryable(&err) {
                return Err(err);
            }
            tracing::warn!(url, attempt, error = %err, "fetch failed; retrying");
            tokio::time::sleep(backoff).await;
            backoff = backoff
                .saturating_mul(self.retry.backoff_factor)
                .min(self.retry.max_backoff);
            attempt += 1;
        }
    }

    /// One capped GET. Returns the body and its declared content type.
    ///
    /// `Range: bytes=0-{max-1}` — a closed range, the only place this client sends one.
    /// A server that ignores it answers 200 with the whole file, so the cap is also
    /// applied locally; without that, "fetch 2 KB to validate" would download a 40 MB
    /// log behind a proxy that strips `Range`.
    async fn body_attempt(&self, url: &str, max: Option<u64>) -> Result<(Vec<u8>, String)> {
        let mut request = self.http.get(url);
        if let Some(max) = max.filter(|max| *max > 0) {
            request = request.header(RANGE, format!("bytes=0-{}", max - 1));
        }
        let response = request
            .send()
            .await
            .map_err(|source| transport_error(url, source))?;

        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();

        if status == StatusCode::NOT_FOUND {
            return Err(Error::LogMissing {
                url: url.to_owned(),
            });
        }
        // A 416 against `bytes=0-` can only mean a zero-length file, which is a real
        // state for a beer that was just created. Empty bytes, not an error.
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            return Ok((Vec::new(), content_type));
        }

        let body = response
            .bytes()
            .await
            .map_err(|source| transport_error(url, source))?;

        if !status.is_success() {
            return Err(Error::HttpStatus {
                url: url.to_owned(),
                status: status.as_u16(),
                preview: preview_bytes(&body),
            });
        }

        let mut body = body.to_vec();
        if let Some(max) = max {
            body.truncate(max as usize);
        }
        Ok((body, content_type))
    }

    async fn attempt(&self, url: &str, offset: u64) -> Result<FetchOutcome> {
        let response = self
            .http
            .get(url)
            // Only ever `bytes=<offset>-` on this path. A suffix range (`bytes=-N`) asks
            // a different question, and nothing about an append-only log ever needs one.
            // (`body_attempt` sends a closed range, but it never resumes.)
            .header(RANGE, format!("bytes={offset}-"))
            .send()
            .await
            .map_err(|source| transport_error(url, source))?;

        let status = response.status();
        let header = |name: reqwest::header::HeaderName| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        let content_range = header(CONTENT_RANGE);
        let content_type = header(CONTENT_TYPE);

        if status == StatusCode::NOT_FOUND {
            // Renamed or deleted. The caller keeps the source and its readings: the
            // beer name is in the filename, so a rename in Fermentrack moves the file,
            // and losing a week of backfill over that would be absurd.
            return Err(Error::LogMissing {
                url: url.to_owned(),
            });
        }

        let body = response
            .bytes()
            .await
            .map_err(|source| transport_error(url, source))?;

        if !status.is_success() && status != StatusCode::RANGE_NOT_SATISFIABLE {
            return Err(Error::HttpStatus {
                url: url.to_owned(),
                status: status.as_u16(),
                preview: preview_bytes(&body),
            });
        }

        // Content check *before* the offset arithmetic, and this order is load-bearing:
        // a 400-byte dashboard fetched against an offset of 50 000 has a body shorter
        // than the offset, so `classify` would call it truncation, 2d would reset the
        // offset to 0, and the next poll would ingest the dashboard from byte 0.
        if status.is_success() {
            check_is_csv(url, &content_type, &body)?;
        }

        let content_range = (!content_range.is_empty()).then_some(content_range);
        let disposition = classify(
            url,
            status.as_u16(),
            content_range.as_deref(),
            offset,
            &body,
        )?;

        Ok(match disposition {
            Disposition::Unchanged { total } => FetchOutcome::Unchanged { total },
            Disposition::Truncated { total } => FetchOutcome::Truncated { total },
            Disposition::Body { skip, total } => {
                let bytes = body[skip..].to_vec();
                if bytes.is_empty() {
                    // A zero-length continuation and a 416 mean the same thing; give
                    // "nothing to do" exactly one shape.
                    FetchOutcome::Unchanged {
                        total: total.unwrap_or(offset),
                    }
                } else {
                    FetchOutcome::Fresh {
                        bytes,
                        offset,
                        total,
                    }
                }
            }
        })
    }
}

fn transport_error(url: &str, source: reqwest::Error) -> Error {
    let url = url.to_owned();
    if source.is_timeout() {
        Error::Timeout { url, source }
    } else {
        Error::Http { url, source }
    }
}

/// Whether one more attempt could plausibly succeed.
///
/// A free function rather than a method on `Error`, because the error type must not
/// know what a retry policy is.
fn is_retryable(err: &Error) -> bool {
    match err {
        Error::Timeout { .. } => true,
        Error::Http { source, .. } => source.is_connect() || source.is_request(),
        Error::HttpStatus { status, .. } => *status == 429 || (500..600).contains(status),
        // A wrong URL is wrong three times too; retrying only makes the wrong answer
        // arrive later and triples the log noise.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "http://fermentrack.local/data/Device%201%20-%20B17%20-%20S_full.csv";

    fn body_of(len: usize) -> Vec<u8> {
        vec![b'x'; len]
    }

    // -- parse_content_range --------------------------------------------------

    #[test]
    fn a_206_content_range_yields_a_start_an_end_and_a_total() {
        assert_eq!(
            parse_content_range("bytes 512-1023/2048"),
            Some(ContentRange::Satisfied {
                start: 512,
                end: 1023,
                total: Some(2048),
            })
        );
    }

    #[test]
    fn a_416_content_range_yields_only_the_total() {
        assert_eq!(
            parse_content_range("bytes */2048"),
            Some(ContentRange::Unsatisfied { total: 2048 })
        );
        // An empty file, which is what a rebuild leaves behind for one poll.
        assert_eq!(
            parse_content_range("bytes */0"),
            Some(ContentRange::Unsatisfied { total: 0 })
        );
    }

    #[test]
    fn a_content_range_with_a_star_total_parses_without_one() {
        assert_eq!(
            parse_content_range("bytes 0-99/*"),
            Some(ContentRange::Satisfied {
                start: 0,
                end: 99,
                total: None,
            })
        );
    }

    #[test]
    fn a_content_range_in_a_unit_other_than_bytes_is_rejected() {
        assert_eq!(parse_content_range("items 0-9/10"), None);
    }

    #[test]
    fn a_content_range_with_no_total_or_a_backwards_range_is_rejected() {
        assert_eq!(parse_content_range("bytes 0-9"), None);
        assert_eq!(parse_content_range("bytes 99-0/100"), None);
        assert_eq!(parse_content_range("bytes a-b/c"), None);
        assert_eq!(parse_content_range("bytes */*"), None);
        assert_eq!(parse_content_range(""), None);
        // An end at or past the total is self-contradictory.
        assert_eq!(parse_content_range("bytes 0-100/100"), None);
    }

    #[test]
    fn content_range_parsing_tolerates_surrounding_whitespace_and_a_capitalised_unit() {
        assert_eq!(
            parse_content_range("  BYTES 0-99/100 "),
            Some(ContentRange::Satisfied {
                start: 0,
                end: 99,
                total: Some(100),
            })
        );
    }

    // -- classify -------------------------------------------------------------

    /// The answer every idle poll gets once the backfill is done. Treating it as
    /// truncation would refetch the whole log once a minute, forever.
    #[test]
    fn a_416_whose_total_equals_the_offset_is_the_idle_steady_state() {
        let got = classify(URL, 416, Some("bytes */4096"), 4096, &[]).unwrap();
        assert_eq!(got, Disposition::Unchanged { total: 4096 });
    }

    /// The log was deleted and started again — a new brew under the same name.
    #[test]
    fn a_416_whose_total_is_below_the_offset_means_the_log_was_recreated() {
        let got = classify(URL, 416, Some("bytes */120"), 4096, &[]).unwrap();
        assert_eq!(got, Disposition::Truncated { total: Some(120) });
    }

    /// The harness cannot produce this, so it is only ever tested here. Without a
    /// length to compare against, the only safe reading is that our offset is gone.
    #[test]
    fn a_416_with_no_content_range_is_treated_as_truncation() {
        assert_eq!(
            classify(URL, 416, None, 4096, &[]).unwrap(),
            Disposition::Truncated { total: None }
        );
        assert_eq!(
            classify(URL, 416, Some("bytes nonsense"), 4096, &[]).unwrap(),
            Disposition::Truncated { total: None }
        );
    }

    /// Resetting here would ask for byte 0, be refused again, and loop forever. Wedge
    /// visibly instead.
    #[test]
    fn a_416_whose_total_is_above_the_offset_is_refused_rather_than_reset() {
        let err = classify(URL, 416, Some("bytes */9000"), 4096, &[]).expect_err("refused");
        assert_eq!(err.code(), "fermentrack.range");
    }

    /// The same truncation as the 416 case, seen from the other side. Also untestable
    /// through the harness.
    #[test]
    fn a_206_whose_total_is_below_the_offset_is_truncation_seen_from_the_other_side() {
        let got = classify(URL, 206, Some("bytes 0-49/50"), 4096, &body_of(50)).unwrap();
        assert_eq!(got, Disposition::Truncated { total: Some(50) });
    }

    /// Trusting this writes rows as though they came from an offset they did not,
    /// which is the one corruption 2d can never detect.
    #[test]
    fn a_206_that_starts_somewhere_other_than_the_offset_is_refused() {
        let err =
            classify(URL, 206, Some("bytes 0-99/9000"), 4096, &body_of(100)).expect_err("refused");
        assert_eq!(err.code(), "fermentrack.range");
        assert!(err.to_string().contains("4096"), "{err}");
    }

    #[test]
    fn a_206_with_no_content_range_is_trusted_to_start_at_the_offset() {
        let got = classify(URL, 206, None, 4096, &body_of(10)).unwrap();
        assert_eq!(
            got,
            Disposition::Body {
                skip: 0,
                total: None
            }
        );
    }

    #[test]
    fn a_200_is_skipped_by_the_offset_locally() {
        let got = classify(URL, 200, None, 4096, &body_of(5000)).unwrap();
        assert_eq!(
            got,
            Disposition::Body {
                skip: 4096,
                total: Some(5000)
            }
        );
    }

    /// Otherwise every idle poll against a Range-ignoring server would hand 2d an
    /// empty chunk and a reason to write.
    #[test]
    fn a_200_exactly_as_long_as_the_offset_is_unchanged_not_an_empty_chunk() {
        let got = classify(URL, 200, None, 4096, &body_of(4096)).unwrap();
        assert_eq!(got, Disposition::Unchanged { total: 4096 });
    }

    /// Not in the spec, and the likelier of the two truncation cases: it is what a
    /// Range-ignoring proxy in front of a rotated log produces.
    #[test]
    fn a_200_shorter_than_the_offset_is_truncation() {
        let got = classify(URL, 200, None, 4096, &body_of(120)).unwrap();
        assert_eq!(got, Disposition::Truncated { total: Some(120) });
    }

    #[test]
    fn a_200_at_offset_zero_is_the_whole_file() {
        let got = classify(URL, 200, None, 0, &body_of(120)).unwrap();
        assert_eq!(
            got,
            Disposition::Body {
                skip: 0,
                total: Some(120)
            }
        );
    }

    // -- check_is_csv ---------------------------------------------------------

    #[test]
    fn a_body_served_as_html_is_refused_even_with_a_two_hundred() {
        let err = check_is_csv(URL, "text/html; charset=utf-8", b"<!DOCTYPE html>")
            .expect_err("not a CSV");
        assert_eq!(err.code(), "fermentrack.not_csv");
        assert!(err.to_string().contains("DOCTYPE"), "{err}");
    }

    /// nginx with no mime mapping sends `text/plain`, or nothing. Neither is a reason
    /// to refuse a real log — but markup in the body still is.
    #[test]
    fn a_mislabelled_body_is_judged_by_its_shape() {
        check_is_csv(URL, "", b"log_time,beer_temp\r\n").expect("no type is fine");
        check_is_csv(URL, "text/plain", b"log_time,beer_temp\r\n").expect("text/plain is fine");
        let err = check_is_csv(URL, "text/plain", b"\n  <html>").expect_err("markup");
        assert_eq!(err.code(), "fermentrack.not_csv");
    }

    // -- log_url --------------------------------------------------------------

    #[test]
    fn a_log_url_percent_encodes_the_spaces_in_a_fermentrack_filename() {
        let got = log_url(
            "http://fermentrack.local",
            "/data/Device 1 - B17 - Seltzer 17_full.csv",
        )
        .unwrap();
        assert_eq!(
            got,
            "http://fermentrack.local/data/Device%201%20-%20B17%20-%20Seltzer%2017_full.csv"
        );
    }

    /// 2f lets the user paste a URL their browser already encoded; encoding it again
    /// turns `%20` into `%2520` and the fetch 404s.
    #[test]
    fn an_already_absolute_remote_path_is_not_encoded_twice() {
        let pasted = "http://other.host/data/Device%201%20-%20B17_full.csv";
        assert_eq!(log_url("http://fermentrack.local", pasted).unwrap(), pasted);
    }

    #[test]
    fn a_remote_path_with_a_leading_double_slash_cannot_redirect_to_another_host() {
        let got = log_url("http://fermentrack.local", "//evil.example/data/x_full.csv").unwrap();
        assert!(got.starts_with("http://fermentrack.local/"), "{got}");
    }

    #[test]
    fn a_remote_path_containing_a_dot_dot_segment_is_refused() {
        let err =
            log_url("http://fermentrack.local", "/data/../../etc/passwd").expect_err("refused");
        assert_eq!(err.code(), "fermentrack.url");
    }

    #[test]
    fn a_base_url_that_is_not_a_url_is_refused_by_name() {
        let err = log_url("not a url", "/data/x_full.csv").expect_err("refused");
        assert_eq!(err.code(), "fermentrack.url");
        let err = log_url("", "/data/x_full.csv").expect_err("refused");
        assert_eq!(err.code(), "fermentrack.url");
    }

    /// A base URL that is itself served under a path must not lose it.
    #[test]
    fn a_base_url_with_a_path_keeps_it() {
        let got = log_url("http://host/fermentrack", "/data/x_full.csv").unwrap();
        assert_eq!(got, "http://host/fermentrack/data/x_full.csv");
    }

    // -- is_retryable ---------------------------------------------------------

    #[test]
    fn a_404_is_not_retried_because_a_wrong_url_is_wrong_three_times_too() {
        assert!(!is_retryable(&Error::LogMissing {
            url: URL.to_owned()
        }));
        assert!(!is_retryable(&Error::NotCsv {
            url: URL.to_owned(),
            content_type: "text/html".to_owned(),
            preview: String::new(),
        }));
    }

    #[test]
    fn a_500_is_retried_and_a_416_is_not() {
        let status = |status| Error::HttpStatus {
            url: URL.to_owned(),
            status,
            preview: String::new(),
        };
        assert!(is_retryable(&status(500)));
        assert!(is_retryable(&status(502)));
        assert!(is_retryable(&status(429)));
        assert!(!is_retryable(&status(400)));
        assert!(!is_retryable(&Error::RangeMismatch {
            url: URL.to_owned(),
            status: 416,
            detail: String::new(),
        }));
    }
}
