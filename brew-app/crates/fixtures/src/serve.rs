//! A mock Fermentrack that serves a log file the way nginx does — and the way it
//! sometimes doesn't.
//!
//! The body lives behind a lock rather than being baked into the mock, so a test can
//! grow the file between two syncs without restarting the server. That is the shape
//! Phase 2e's growing-file test needs: serve N bytes, sync, append M, sync again,
//! assert no duplicates and no gaps.
//!
//! Both `Range` behaviours are here because both happen in the wild. nginx honours
//! `Range` and answers `206`; a proxy, a `gzip` content-encoding, or a Django dev
//! server in front of `/data/` will quietly ignore it and answer `200` with the whole
//! file. A client that assumes `206` silently duplicates every row it has already
//! stored the first time it meets the second kind.

use std::sync::{Arc, Mutex, PoisonError};

use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::any};

/// What the server does with a `Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RangeMode {
    /// nginx: `206` plus `Content-Range`, or `416` past the end.
    #[default]
    Honour,
    /// Range ignored — always `200` with the whole body.
    Ignore,
}

/// A failure to serve instead of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fault {
    #[default]
    None,
    /// The log was renamed or deleted. Must not wipe the readings we already have.
    NotFound,
    /// A misdirected URL landing on Fermentrack's web UI: `200`, `text/html`, and
    /// not a CSV in sight.
    Html,
    ServerError,
}

struct ServeState {
    body: Vec<u8>,
    mode: RangeMode,
    fault: Fault,
    requests: usize,
    last_range: Option<String>,
}

/// A running mock. Dropping it shuts the server down.
pub struct FixtureServer {
    server: MockServer,
    state: Arc<Mutex<ServeState>>,
    path: String,
}

impl FixtureServer {
    /// Start serving `body` at `path` (e.g. `/data/Device 1 - B17 - Seltzer 17_full.csv`).
    pub async fn start(path: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        let path = path.into();
        let state = Arc::new(Mutex::new(ServeState {
            body: body.into(),
            mode: RangeMode::default(),
            fault: Fault::default(),
            requests: 0,
            last_range: None,
        }));

        let server = MockServer::start().await;
        // `any()` rather than wiremock's `path()` matcher: the log filename contains
        // spaces, so the matcher would have to agree with us about percent-encoding.
        // Deciding that inside the responder means a wrong path returns a 404 the
        // test can see, instead of wiremock's "no mock matched" panic.
        Mock::given(any())
            .respond_with(Responder {
                state: Arc::clone(&state),
                path: path.clone(),
            })
            .mount(&server)
            .await;

        Self {
            server,
            state,
            path,
        }
    }

    /// The full URL of the served file, percent-encoded.
    pub fn url(&self) -> String {
        format!("{}{}", self.server.uri(), encode_path(&self.path))
    }

    /// The server root, for tests that need to request something else.
    pub fn base_url(&self) -> String {
        self.server.uri()
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut ServeState) -> T) -> T {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Replace the whole body — a rebuild, or a file that was recreated under us.
    pub fn set_body(&self, body: impl Into<Vec<u8>>) {
        self.with_state(|s| s.body = body.into());
    }

    /// Append, the way Fermentrack's `writerow` does.
    pub fn append(&self, bytes: impl AsRef<[u8]>) {
        self.with_state(|s| s.body.extend_from_slice(bytes.as_ref()));
    }

    /// Shrink the file. The log was rotated, and every stored byte offset is now a
    /// lie.
    pub fn truncate_to(&self, len: usize) {
        self.with_state(|s| s.body.truncate(len));
    }

    pub fn len(&self) -> usize {
        self.with_state(|s| s.body.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn set_mode(&self, mode: RangeMode) {
        self.with_state(|s| s.mode = mode);
    }

    pub fn set_fault(&self, fault: Fault) {
        self.with_state(|s| s.fault = fault);
    }

    /// Requests seen, including the ones that 404'd. A sync that fires twice per tick
    /// is a bug the row counts cannot show.
    pub fn request_count(&self) -> usize {
        self.with_state(|s| s.requests)
    }

    /// The `Range` header of the most recent request, verbatim, or `None` if it sent
    /// none.
    ///
    /// Exposed because `parse_range` accepts only the forms our client produces:
    /// anything else falls through to a whole-file `200`, which a client that applies
    /// the offset locally then turns into *correct-looking* output. Without this, a
    /// regression that stopped sending the header at all would pass every other test
    /// in the suite.
    pub fn last_range(&self) -> Option<String> {
        self.with_state(|s| s.last_range.clone())
    }
}

struct Responder {
    state: Arc<Mutex<ServeState>>,
    path: String,
}

impl Respond for Responder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.requests += 1;
        state.last_range = request
            .headers
            .get("range")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        if decode_path(request.url.path()) != self.path {
            return not_found();
        }

        match state.fault {
            Fault::NotFound => return not_found(),
            Fault::Html => {
                return ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/html; charset=utf-8")
                    .set_body_string(
                        "<!DOCTYPE html><html><head><title>Fermentrack</title></head>\
                         <body><h1>Dashboard</h1></body></html>",
                    );
            }
            Fault::ServerError => {
                return ResponseTemplate::new(500).set_body_string("Internal Server Error");
            }
            Fault::None => {}
        }

        let total = state.body.len();
        let range = request
            .headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_range);

        match (state.mode, range) {
            // No `Range`, or a server that ignores it: the whole file.
            (RangeMode::Ignore, _) | (RangeMode::Honour, None) => whole(&state.body),

            (RangeMode::Honour, Some((start, end))) => {
                if start >= total {
                    // What a truncated or recreated file looks like from the client's
                    // side: the offset we saved is past the end of the file that is
                    // there now.
                    return ResponseTemplate::new(416)
                        .insert_header("Content-Range", format!("bytes */{total}").as_str())
                        .insert_header("Accept-Ranges", "bytes");
                }
                let end = end.unwrap_or(total - 1).min(total - 1);
                let body = state.body[start..=end].to_vec();
                ResponseTemplate::new(206)
                    .insert_header("Content-Type", "text/csv")
                    .insert_header("Accept-Ranges", "bytes")
                    .insert_header(
                        "Content-Range",
                        format!("bytes {start}-{end}/{total}").as_str(),
                    )
                    .set_body_bytes(body)
            }
        }
    }
}

fn whole(body: &[u8]) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("Content-Type", "text/csv")
        .insert_header("Accept-Ranges", "bytes")
        .set_body_bytes(body.to_vec())
}

fn not_found() -> ResponseTemplate {
    ResponseTemplate::new(404).set_body_string("Not Found")
}

/// `bytes=N-` and `bytes=N-M`. Suffix ranges (`bytes=-N`) are not supported: our
/// client never sends one, and answering a form we don't produce would hide the day
/// we accidentally start.
fn parse_range(value: &str) -> Option<(usize, Option<usize>)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: usize = start.trim().parse().ok()?;
    let end = match end.trim() {
        "" => None,
        e => Some(e.parse().ok()?),
    };
    Some((start, end))
}

/// Percent-encode a path, leaving `/` and the unreserved set alone. Enough for
/// Fermentrack's filenames, which are `^[a-zA-Z0-9 _-]*$` plus the separators we put
/// there ourselves.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn decode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_headers_parse() {
        assert_eq!(parse_range("bytes=0-"), Some((0, None)));
        assert_eq!(parse_range("bytes=512-"), Some((512, None)));
        assert_eq!(parse_range("bytes=10-20"), Some((10, Some(20))));
        assert_eq!(parse_range("bytes=-20"), None); // suffix range, unsupported
        assert_eq!(parse_range("items=0-"), None);
        assert_eq!(parse_range("bytes=abc-"), None);
    }

    #[test]
    fn paths_survive_a_round_trip() {
        let path = "/data/Device 1 - B17 - Seltzer 17_full.csv";
        assert_eq!(
            encode_path(path),
            "/data/Device%201%20-%20B17%20-%20Seltzer%2017_full.csv"
        );
        assert_eq!(decode_path(&encode_path(path)), path);
    }
}
