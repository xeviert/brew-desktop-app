//! The mock Fermentrack's own tests.
//!
//! A test rig nobody tests is a rig that agrees with whatever the client does. These
//! assert the server behaves like nginx serving `/data/` — and, in `Ignore` mode,
//! like the proxies that don't.

use brewfixture::{Fault, FixtureServer, Kind, LogSpec, RangeMode, Scenario};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT_RANGES, CONTENT_RANGE, RANGE};

fn fixture() -> (LogSpec, String) {
    let spec = LogSpec {
        rows: 20,
        ..Scenario::Gravity.spec()
    };
    let body = spec.full_csv();
    (spec, body)
}

async fn start() -> (FixtureServer, String) {
    let (spec, body) = fixture();
    let server = FixtureServer::start(spec.remote_path(Kind::Full), body.clone()).await;
    (server, body)
}

#[tokio::test]
async fn a_plain_get_returns_the_whole_file() {
    let (server, body) = start().await;

    let response = reqwest::get(server.url()).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[ACCEPT_RANGES], "bytes");
    assert_eq!(response.text().await.unwrap(), body);
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn a_range_request_returns_206_and_only_the_tail() {
    let (server, body) = start().await;
    let offset = 200;

    let response = reqwest::Client::new()
        .get(server.url())
        .header(RANGE, format!("bytes={offset}-"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers()[CONTENT_RANGE],
        format!("bytes {offset}-{}/{}", body.len() - 1, body.len()).as_str()
    );
    assert_eq!(response.text().await.unwrap(), body[offset..]);
}

/// The whole point of the offset resume: a second fetch must carry only the bytes
/// that were appended since the first.
#[tokio::test]
async fn growing_the_file_serves_only_the_new_bytes() {
    let (server, body) = start().await;
    let first_len = body.len();

    let (spec, _) = fixture();
    let more = LogSpec {
        start: spec.start + (spec.rows as i64) * spec.interval_secs,
        seed: 99,
        rows: 5,
        ..spec
    };
    // Header-less: this is an append to an existing file, not a new one.
    let appended: String = more.full_csv().split_inclusive("\r\n").skip(1).collect();
    server.append(&appended);

    let response = reqwest::Client::new()
        .get(server.url())
        .header(RANGE, format!("bytes={first_len}-"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.text().await.unwrap(), appended);
    assert_eq!(server.len(), first_len + appended.len());
}

/// A stored offset past the end of the file: the log was rotated or recreated, and
/// the client has to notice rather than resume into nothing.
#[tokio::test]
async fn an_offset_past_the_end_is_416_with_the_real_length() {
    let (server, body) = start().await;
    server.truncate_to(100);

    let response = reqwest::Client::new()
        .get(server.url())
        .header(RANGE, format!("bytes={}-", body.len()))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()[CONTENT_RANGE], "bytes */100");
}

#[tokio::test]
async fn a_closed_range_is_honoured_and_clamped_to_the_end() {
    let (server, body) = start().await;

    let exact = reqwest::Client::new()
        .get(server.url())
        .header(RANGE, "bytes=0-9")
        .send()
        .await
        .unwrap();
    assert_eq!(exact.text().await.unwrap(), body[0..=9]);

    let past_end = reqwest::Client::new()
        .get(server.url())
        .header(RANGE, format!("bytes=0-{}", body.len() + 1000))
        .send()
        .await
        .unwrap();
    assert_eq!(past_end.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(past_end.text().await.unwrap(), body);
}

/// The failure mode that silently duplicates rows: a server that ignores `Range` and
/// answers `200` with the whole file. The client must skip the offset locally.
#[tokio::test]
async fn ignore_mode_returns_200_and_the_whole_body_despite_the_range_header() {
    let (server, body) = start().await;
    server.set_mode(RangeMode::Ignore);

    let response = reqwest::Client::new()
        .get(server.url())
        .header(RANGE, "bytes=200-")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(CONTENT_RANGE).is_none());
    assert_eq!(response.text().await.unwrap(), body);
}

#[tokio::test]
async fn faults_are_servable() {
    let (server, _) = start().await;

    server.set_fault(Fault::NotFound);
    let response = reqwest::get(server.url()).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    server.set_fault(Fault::Html);
    let response = reqwest::get(server.url()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .starts_with("<!DOCTYPE html>")
    );

    server.set_fault(Fault::ServerError);
    let response = reqwest::get(server.url()).await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    server.set_fault(Fault::None);
    let response = reqwest::get(server.url()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(server.request_count(), 4);
}

/// A renamed log — the beer name is in the filename — must 404 rather than quietly
/// serving some other file.
#[tokio::test]
async fn another_path_is_404_not_a_panic() {
    let (server, _) = start().await;

    let response = reqwest::get(format!(
        "{}/data/Device 9 - B9 - Nope_full.csv",
        server.base_url()
    ))
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// The filename has spaces in it, so the URL the server hands out has to be encoded
/// and its own matching has to decode. If this breaks, every other test here passes
/// against a 404.
#[tokio::test]
async fn the_served_url_survives_the_spaces_in_a_fermentrack_filename() {
    let (server, body) = start().await;

    assert!(server.url().contains("%20"));
    let response = reqwest::get(server.url()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap().len(), body.len());
}
