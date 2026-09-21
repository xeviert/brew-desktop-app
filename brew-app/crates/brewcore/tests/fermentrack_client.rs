//! Phase 2c's client against the mock Fermentrack.
//!
//! The pure decisions — how a `Content-Range` is read, what each status means for a
//! stored offset — are unit-tested inside `fermentrack::client`, where they need no
//! server. These are the other half: that the client really sends `bytes=<offset>-`,
//! really reads the answer, and really refuses a body that is not a log. Real sockets,
//! real `reqwest`, real generated `_full.csv` bytes.
//!
//! No live Fermentrack is involved, and none is needed. What these cannot prove is
//! that the fixtures are honest about a real box; that is Phase 0's outstanding
//! validation step, scheduled for the end of Phase 2.

use std::time::Duration;

use brewcore::fermentrack::client::{Client, ClientSettings, FetchOutcome, RetryPolicy, log_url};
use brewfixture::{Fault, FixtureServer, Kind, LogSpec, RangeMode, Scenario};

/// Retry off. Anything not testing the retry loop wants one attempt and no sleeping.
fn client() -> Client {
    Client::with_settings(ClientSettings {
        retry: RetryPolicy {
            attempts: 1,
            ..RetryPolicy::default()
        },
        ..ClientSettings::default()
    })
    .expect("build the client")
}

/// Retry on, with a backoff small enough that three attempts cost under a millisecond.
/// Deliberately a real duration rather than a paused clock: auto-advance with live
/// sockets in flight is flaky, and a real sleep proves the field is actually wired.
fn retrying_client(attempts: u32) -> Client {
    Client::with_settings(ClientSettings {
        retry: RetryPolicy {
            attempts,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
            ..RetryPolicy::default()
        },
        ..ClientSettings::default()
    })
    .expect("build the client")
}

fn spec(rows: usize) -> LogSpec {
    LogSpec {
        rows,
        ..Scenario::Active.spec()
    }
}

async fn start(rows: usize) -> (FixtureServer, String) {
    let spec = spec(rows);
    let body = spec.full_csv();
    let server = FixtureServer::start(spec.remote_path(Kind::Full), body.clone()).await;
    (server, body)
}

/// The rows a longer run of the same log would have added, header stripped — what
/// Fermentrack's `writerow` appends.
fn appended_rows(from: usize, to: usize) -> String {
    let longer = spec(to).full_csv();
    let shorter_rows = spec(from).full_csv().split_inclusive("\r\n").count();
    longer
        .split_inclusive("\r\n")
        .skip(shorter_rows)
        .collect::<String>()
}

fn fresh(outcome: FetchOutcome) -> Vec<u8> {
    match outcome {
        FetchOutcome::Fresh { bytes, .. } => bytes,
        other => panic!("expected new bytes, got {other:?}"),
    }
}

#[tokio::test]
async fn a_first_fetch_from_zero_returns_the_whole_body() {
    let (server, body) = start(20).await;

    let got = client().fetch(&server.url(), 0).await.unwrap();

    assert_eq!(fresh(got), body.as_bytes());
    assert_eq!(server.request_count(), 1);
}

/// The transport half of the growing-file test. The write-path half, which proves no
/// duplicate and no missing rows, lands with 2e.
#[tokio::test]
async fn a_resume_fetches_only_the_bytes_appended_since_the_last_sync() {
    let (server, body) = start(20).await;
    let offset = body.len() as u64;

    let added = appended_rows(20, 25);
    server.append(&added);

    let got = client().fetch(&server.url(), offset).await.unwrap();

    assert_eq!(fresh(got), added.as_bytes());
}

/// Some proxies answer `200` with the whole file however politely you ask. The offset
/// then has to be applied locally, or every poll re-ingests the entire log.
#[tokio::test]
async fn a_server_that_ignores_range_still_yields_only_the_new_bytes() {
    let (server, body) = start(20).await;
    server.set_mode(RangeMode::Ignore);
    let offset = body.len() as u64;

    let added = appended_rows(20, 25);
    server.append(&added);

    let got = client().fetch(&server.url(), offset).await.unwrap();

    assert_eq!(fresh(got), added.as_bytes());
}

/// What every poll returns once the backfill is done. If this ever came back `Fresh`,
/// the app would refetch the whole log once a minute for the life of the batch.
#[tokio::test]
async fn an_idle_poll_against_an_unchanged_file_is_a_no_op_not_a_refetch() {
    let (server, body) = start(20).await;
    let offset = body.len() as u64;

    let got = client().fetch(&server.url(), offset).await.unwrap();

    assert_eq!(got, FetchOutcome::Unchanged { total: offset });
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn a_truncated_log_asks_the_caller_to_start_over() {
    let (server, body) = start(20).await;
    let offset = body.len() as u64;
    server.truncate_to(120);

    let got = client().fetch(&server.url(), offset).await.unwrap();

    assert_eq!(got, FetchOutcome::Truncated { total: Some(120) });
}

/// The beer name is in the filename, so a rename in Fermentrack moves the file. The
/// caller keeps the source and every row it already has; only the URL needs fixing.
#[tokio::test]
async fn a_missing_log_is_an_error_that_names_the_url() {
    let (server, _) = start(20).await;
    server.set_fault(Fault::NotFound);

    let err = client()
        .fetch(&server.url(), 0)
        .await
        .expect_err("the log is gone");

    assert_eq!(err.code(), "fermentrack.log_missing");
    assert!(err.to_string().contains(&server.url()), "{err}");
}

/// A URL pointing at the dashboard answers **200**, so a status check alone misses it.
/// Left to the parser, a resume would read it as zero rows and a clean report, and the
/// offset would advance past the whole page.
#[tokio::test]
async fn an_html_dashboard_served_with_a_200_is_refused_before_it_reaches_the_parser() {
    let (server, _) = start(20).await;
    server.set_fault(Fault::Html);

    let err = client()
        .fetch(&server.url(), 0)
        .await
        .expect_err("not a CSV");

    assert_eq!(err.code(), "fermentrack.not_csv");
    assert!(err.to_string().contains("DOCTYPE"), "{err}");
}

/// The content check must run before the offset arithmetic. A short dashboard fetched
/// at a large offset is shorter than that offset, so classification alone would call
/// it truncation — the offset would reset to 0 and the next poll would ingest the page.
#[tokio::test]
async fn an_html_dashboard_shorter_than_the_offset_is_not_mistaken_for_a_truncated_log() {
    let (server, body) = start(20).await;
    let offset = body.len() as u64;
    server.set_fault(Fault::Html);

    let err = client()
        .fetch(&server.url(), offset)
        .await
        .expect_err("not a CSV");

    assert_eq!(err.code(), "fermentrack.not_csv");
}

#[tokio::test]
async fn a_server_error_is_retried_and_then_surfaced_with_the_first_bytes_of_the_body() {
    let (server, _) = start(20).await;
    server.set_fault(Fault::ServerError);

    let err = retrying_client(3)
        .fetch(&server.url(), 0)
        .await
        .expect_err("500");

    assert_eq!(err.code(), "fermentrack.http_status");
    assert!(err.to_string().contains("Internal Server Error"), "{err}");
    assert_eq!(server.request_count(), 3);
}

/// A wrong URL is wrong three times too. Retrying it only makes the wrong answer
/// arrive later and triples the log noise.
#[tokio::test]
async fn a_404_is_not_retried() {
    let (server, _) = start(20).await;
    server.set_fault(Fault::NotFound);

    let err = retrying_client(3)
        .fetch(&server.url(), 0)
        .await
        .expect_err("404");

    assert_eq!(err.code(), "fermentrack.log_missing");
    assert_eq!(server.request_count(), 1);
}

/// A Fermentrack that is switched off must fail, not hang the UI. Port 1 on loopback
/// refuses immediately; the assertion is on the error, never on a duration, because
/// timing assertions flake on CI.
#[tokio::test]
async fn a_fetch_against_a_closed_port_fails_instead_of_hanging() {
    let url = log_url("http://127.0.0.1:1", &spec(1).remote_path(Kind::Full)).unwrap();

    let err = client()
        .fetch(&url, 0)
        .await
        .expect_err("nothing listening");

    assert_eq!(err.code(), "fermentrack.http");
}

/// Fermentrack's filenames contain spaces. If the client and the server ever disagree
/// about encoding them, every other test here is silently passing against a 404.
#[tokio::test]
async fn the_url_the_client_builds_is_the_url_the_server_serves() {
    let (server, _) = start(1).await;

    let built = log_url(&server.base_url(), server.path()).unwrap();

    assert_eq!(built, server.url());
    assert!(client().fetch(&built, 0).await.is_ok());
}

/// The one wire-level property nothing else can catch: the rig honours only the forms
/// we produce, and any other shape falls through to a whole-file `200` that the
/// client's own local-skip then turns into correct-looking output.
#[tokio::test]
async fn the_client_asks_for_exactly_bytes_offset_dash() {
    let (server, _) = start(20).await;

    client().fetch(&server.url(), 512).await.unwrap();

    assert_eq!(server.last_range().as_deref(), Some("bytes=512-"));
}
