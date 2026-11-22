//! The desktop app's side of `brewd`. LAN-only.
//!
//! Reads are open, the trust model Fermentrack's `/data/` had. `GET /journals/` lists
//! every journal, rotated ones included: the listing Fermentrack never had.
//! `GET /journals/<name>` serves one through `tower-http`'s `ServeFile`, which answers
//! `Range` with `206` + `Content-Range`, or `416` past the end — the two shapes
//! `brewcore::journal::Client` resumes from. Phase 6 adds `GET /watches`,
//! `GET /alerts` and `GET /alerts/log`, the last served the same way.
//!
//! Writes are Phase 6's, and the first `brewd` has: `PUT /watches`,
//! `POST /alerts/<id>/ack`, `POST /alerts/<id>/snooze` and `POST /notify/test`. Each
//! needs `Authorization: Bearer <http.token>`. A wrong or missing token is `401`; a
//! `brewd` with no token configured refuses every write with `403`, so an upgrade
//! never opens writes to the LAN by default.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use brewcore::alert::{Alert, WatchSet};
use brewcore::journal::{JournalEntry, is_journal_name};
use brewcore::units::Timestamp;
use serde::Deserialize;
use tokio::sync::Notify;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::alerts::{self, Shared};
use crate::notify::Ntfy;

#[derive(Clone)]
pub struct AppState {
    pub journal_dir: Arc<PathBuf>,
    pub alert_log: Arc<PathBuf>,
    pub alerts: Shared,
    pub token: Option<Arc<str>>,
    pub ntfy: Option<Ntfy>,
    /// Tells [`alerts::run`] something may be owed a notification.
    pub wake: Arc<Notify>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/journals", get(list))
        .route("/journals/", get(list))
        .route("/journals/{name}", get(journal))
        .route("/watches", get(get_watches).put(put_watches))
        .route("/alerts", get(get_alerts))
        .route("/alerts/log", get(alert_log))
        .route("/alerts/{id}/ack", post(ack))
        .route("/alerts/{id}/snooze", post(snooze))
        .route("/notify/test", post(notify_test))
        .with_state(state)
}

type Refusal = (StatusCode, String);

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), Refusal> {
    let Some(token) = &state.token else {
        return Err((
            StatusCode::FORBIDDEN,
            "brewd has no http.token set, so writes are off".into(),
        ));
    };
    let given = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match given {
        Some(given) if same(given.as_bytes(), token.as_bytes()) => Ok(()),
        _ => Err((StatusCode::UNAUTHORIZED, "wrong or missing token".into())),
    }
}

/// Every byte compared whatever the first mismatch, so the time taken says nothing
/// about how much of a guess was right.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn get_watches(State(state): State<AppState>) -> Json<WatchSet> {
    Json(alerts::lock(&state.alerts).watches().clone())
}

async fn put_watches(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, Refusal> {
    authorize(&state, &headers)?;
    let set = WatchSet::from_json(&body).map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{}: {e}", e.code()),
        )
    })?;
    let raised = alerts::lock(&state.alerts)
        .set_watches(set, Timestamp::now())
        .map_err(|e| {
            tracing::error!(error = %e, "cannot save watches");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot save watches".into(),
            )
        })?;
    if raised {
        state.wake.notify_one();
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn get_alerts(State(state): State<AppState>) -> Json<Vec<Alert>> {
    Json(alerts::lock(&state.alerts).alerts().to_vec())
}

async fn alert_log(State(state): State<AppState>, request: Request) -> Response {
    match ServeFile::new(&*state.alert_log).oneshot(request).await {
        Ok(response) => response.map(Body::new),
        Err(never) => match never {},
    }
}

async fn ack(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    headers: HeaderMap,
) -> Result<StatusCode, Refusal> {
    authorize(&state, &headers)?;
    if alerts::lock(&state.alerts).ack(id, Timestamp::now()) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, format!("alert {id} is not raised")))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snooze {
    until: Timestamp,
}

async fn snooze(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, Refusal> {
    authorize(&state, &headers)?;
    let Snooze { until } = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    if alerts::lock(&state.alerts).snooze(id, until, Timestamp::now()) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, format!("alert {id} is not raised")))
    }
}

async fn notify_test(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, Refusal> {
    authorize(&state, &headers)?;
    let Some(ntfy) = &state.ntfy else {
        return Err((StatusCode::CONFLICT, "brewd has no [notify] section".into()));
    };
    ntfy.send(
        "brewd: test",
        "Alerts from brewd will arrive like this.",
        3,
        &[],
    )
    .await
    .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<JournalEntry>>, StatusCode> {
    let dir = &state.journal_dir;
    let internal = |e: std::io::Error| {
        tracing::error!(dir = %dir.display(), error = %e, "cannot list journals");
        StatusCode::INTERNAL_SERVER_ERROR
    };

    let mut entries = Vec::new();
    let mut read = tokio::fs::read_dir(dir.as_path()).await.map_err(internal)?;
    while let Some(entry) = read.next_entry().await.map_err(internal)? {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !is_journal_name(&name) {
            continue;
        }
        let meta = entry.metadata().await.map_err(internal)?;
        if !meta.is_file() {
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        entries.push(JournalEntry {
            name,
            bytes: meta.len(),
            modified: Timestamp::from_secs(modified),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(entries))
}

async fn journal(
    State(state): State<AppState>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    // `Path` has already percent-decoded, so `..%2F` arrives as `../` and fails here.
    if !is_journal_name(&name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match ServeFile::new(state.journal_dir.join(&name))
        .oneshot(request)
        .await
    {
        Ok(response) => response.map(Body::new),
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::Alerts;
    use crate::journal::Journal;
    use brewcore::journal::{Client, FetchOutcome};
    use reqwest::header::{CONTENT_RANGE, RANGE};
    use std::net::SocketAddr;

    const T0: Timestamp = Timestamp::from_secs(1_790_000_000);

    const TOKEN: &str = "0123456789abcdef";

    /// State in a directory of its own beside `journal_dir`'s, so the alert log never
    /// lands among the journals.
    fn app(journal_dir: PathBuf, token: Option<&str>, ntfy: Option<Ntfy>) -> AppState {
        let state_dir = tempfile::tempdir().unwrap().keep();
        let alerts = Alerts::open(&state_dir, &journal_dir, &[], T0).unwrap();
        AppState {
            journal_dir: Arc::new(journal_dir),
            alert_log: Arc::new(state_dir.join("alerts.csv")),
            alerts: Arc::new(std::sync::Mutex::new(alerts)),
            token: token.map(Arc::from),
            ntfy,
            wake: Arc::new(Notify::new()),
        }
    }

    async fn serve_app(app: AppState) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(app)).await.unwrap() });
        addr
    }

    async fn serve(dir: PathBuf) -> SocketAddr {
        serve_app(app(dir, None, None)).await
    }

    fn some(values: &[&str]) -> Vec<Option<String>> {
        values.iter().map(|v| Some((*v).to_owned())).collect()
    }

    #[tokio::test]
    async fn the_listing_shows_journals_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.csv"), "time,x\n").unwrap();
        std::fs::write(dir.path().join("a.1790000000.csv"), "time\n").unwrap();
        std::fs::write(dir.path().join("brewd.toml"), "").unwrap();
        std::fs::write(dir.path().join(".hidden.csv"), "").unwrap();
        std::fs::create_dir(dir.path().join("sub.csv")).unwrap();
        let addr = serve(dir.path().to_owned()).await;

        for path in ["/journals/", "/journals"] {
            let body = reqwest::get(format!("http://{addr}{path}"))
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            let entries: Vec<JournalEntry> = serde_json::from_slice(&body).unwrap();
            let names: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.bytes)).collect();
            assert_eq!(names, [("a.1790000000.csv", 5), ("b.csv", 7)], "{path}");
        }
    }

    #[tokio::test]
    async fn a_range_request_gets_a_206_and_past_the_end_a_416() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("d.csv"), "time,a\n1790000000,1\n").unwrap();
        let addr = serve(dir.path().to_owned()).await;
        let url = format!("http://{addr}/journals/d.csv");
        let http = reqwest::Client::new();

        let partial = http
            .get(&url)
            .header(RANGE, "bytes=7-")
            .send()
            .await
            .unwrap();
        assert_eq!(partial.status(), 206);
        assert_eq!(partial.headers()[CONTENT_RANGE], "bytes 7-19/20");
        assert_eq!(partial.text().await.unwrap(), "1790000000,1\n");

        let past = http
            .get(&url)
            .header(RANGE, "bytes=20-")
            .send()
            .await
            .unwrap();
        assert_eq!(past.status(), 416);
        assert_eq!(past.headers()[CONTENT_RANGE], "bytes */20");
    }

    #[tokio::test]
    async fn anything_but_a_journal_name_is_a_404() {
        let dir = tempfile::tempdir().unwrap();
        let journals = dir.path().join("journals");
        std::fs::create_dir(&journals).unwrap();
        std::fs::write(dir.path().join("secret.csv"), "no").unwrap();
        std::fs::write(journals.join("notes.txt"), "no").unwrap();
        let addr = serve(journals).await;

        for path in [
            "/journals/..%2Fsecret.csv",
            "/journals/..%5Csecret.csv",
            "/journals/notes.txt",
            "/journals/missing.csv",
        ] {
            let status = reqwest::get(format!("http://{addr}{path}"))
                .await
                .unwrap()
                .status();
            assert_eq!(status, 404, "{path}");
        }
    }

    /// The consumer this exists for: the desktop's own client, resuming by offset
    /// against a journal that is growing underneath it.
    #[tokio::test]
    async fn the_desktop_client_backfills_then_resumes_a_growing_journal() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal =
            Journal::open(dir.path(), "ispindel-1", &["tilt", "gravity"], T0).unwrap();
        journal.append(T0, &some(&["45.2", "1.012"])).unwrap();
        let addr = serve(dir.path().to_owned()).await;
        let url = format!("http://{addr}/journals/ispindel-1.csv");
        let client = Client::new().unwrap();

        let FetchOutcome::Fresh {
            bytes, offset: 0, ..
        } = client.fetch(&url, 0).await.unwrap()
        else {
            panic!("expected the whole journal");
        };
        assert_eq!(bytes, b"time,tilt,gravity\n1790000000,45.2,1.012\n");
        let offset = bytes.len() as u64;

        assert!(matches!(
            client.fetch(&url, offset).await.unwrap(),
            FetchOutcome::Unchanged { total } if total == offset
        ));

        journal
            .append(T0.saturating_add_secs(900), &some(&["44.9", "1.011"]))
            .unwrap();
        let FetchOutcome::Fresh {
            bytes, offset: at, ..
        } = client.fetch(&url, offset).await.unwrap()
        else {
            panic!("expected the appended record");
        };
        assert_eq!(at, offset);
        assert_eq!(bytes, b"1790000900,44.9,1.011\n");
    }

    #[tokio::test]
    async fn writes_need_the_token_and_are_off_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let body = serde_json::to_vec(&WatchSet::new(vec![])).unwrap();
        let http = reqwest::Client::new();

        let open = serve_app(app(dir.path().to_owned(), None, None)).await;
        let off = http
            .put(format!("http://{open}/watches"))
            .bearer_auth(TOKEN)
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(off.status(), 403);

        let locked = serve_app(app(dir.path().to_owned(), Some(TOKEN), None)).await;
        let url = format!("http://{locked}/watches");
        for auth in [None, Some("0123456789abcdeX"), Some("short")] {
            let mut request = http.put(&url).body(body.clone());
            if let Some(auth) = auth {
                request = request.bearer_auth(auth);
            }
            assert_eq!(request.send().await.unwrap().status(), 401, "{auth:?}");
        }
        let ok = http
            .put(&url)
            .bearer_auth(TOKEN)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 204);
        assert_eq!(
            reqwest::get(&url).await.unwrap().status(),
            200,
            "reads stay open"
        );
    }

    #[tokio::test]
    async fn pushed_watches_are_served_back_and_a_bad_set_is_a_422() {
        let dir = tempfile::tempdir().unwrap();
        let addr = serve_app(app(dir.path().to_owned(), Some(TOKEN), None)).await;
        let url = format!("http://{addr}/watches");
        let http = reqwest::Client::new();

        let set = WatchSet::new(vec![crate::alerts::tests::watch()]);
        let put = http
            .put(&url)
            .bearer_auth(TOKEN)
            .body(serde_json::to_vec(&set).unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(put.status(), 204);
        let back: WatchSet = reqwest::get(&url).await.unwrap().json_bytes().await;
        assert_eq!(back, set);

        let bad = http
            .put(&url)
            .bearer_auth(TOKEN)
            .body(format!(r#"{{"v":{},"watches":[]}}"#, brewcore::alert::WATCH_VERSION + 1))
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), 422);
        assert!(bad.text().await.unwrap().starts_with("alert.watch_version"));
    }

    #[tokio::test]
    async fn ack_and_snooze_answer_404_for_an_alert_not_raised() {
        let dir = tempfile::tempdir().unwrap();
        let addr = serve_app(app(dir.path().to_owned(), Some(TOKEN), None)).await;
        let http = reqwest::Client::new();
        let ack = http
            .post(format!("http://{addr}/alerts/5/ack"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(ack.status(), 404);
        let snooze = http
            .post(format!("http://{addr}/alerts/5/snooze"))
            .bearer_auth(TOKEN)
            .body(r#"{"until":1790000000}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(snooze.status(), 404);
        let alerts: Vec<Alert> = reqwest::get(format!("http://{addr}/alerts"))
            .await
            .unwrap()
            .json_bytes()
            .await;
        assert!(alerts.is_empty());
    }

    #[tokio::test]
    async fn the_alert_log_resumes_like_a_journal() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path().to_owned(), None, None);
        let addr = serve_app(app.clone()).await;
        let client = Client::new().unwrap();
        let url = format!("http://{addr}/alerts/log");

        let FetchOutcome::Fresh { bytes, .. } = client.fetch(&url, 0).await.unwrap() else {
            panic!("expected the header");
        };
        assert_eq!(
            bytes,
            format!("{}\n", brewcore::alert::LOG_HEADER).as_bytes()
        );
    }

    #[tokio::test]
    async fn the_test_notification_reaches_ntfy_or_says_why_not() {
        let dir = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();

        let none = serve_app(app(dir.path().to_owned(), Some(TOKEN), None)).await;
        let res = http
            .post(format!("http://{none}/notify/test"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 409);

        let (config, seen) = crate::notify::tests::fake_ntfy(StatusCode::OK).await;
        let ntfy = Ntfy::new(&config).unwrap();
        let some = serve_app(app(dir.path().to_owned(), Some(TOKEN), Some(ntfy))).await;
        let res = http
            .post(format!("http://{some}/notify/test"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 204);
        assert_eq!(seen.lock().unwrap()[0]["title"], "brewd: test");
    }

    trait JsonBytes {
        async fn json_bytes<T: serde::de::DeserializeOwned>(self) -> T;
    }

    impl JsonBytes for reqwest::Response {
        async fn json_bytes<T: serde::de::DeserializeOwned>(self) -> T {
            serde_json::from_slice(&self.bytes().await.unwrap()).unwrap()
        }
    }
}
