//! Phase 2f against the mock Fermentrack: a pasted URL becomes a source, or says why not.
//!
//! The pure halves are unit-tested where they live — filename parsing and the
//! encode/decode round trip in `fermentrack::discovery`, header classification in
//! `fermentrack::csv_log`. These are the parts that need a socket and a database: that
//! the probe really caps its read, that each wrong URL produces its own `code()`, and
//! that a source added here is one 2c–2e can actually sync.
//!
//! No live Fermentrack is involved. What these cannot prove is that the fixtures are
//! honest about a real box; that is Phase 0's outstanding validation step.

use brewcore::{
    db::{Db, SourceKind, tier1},
    fermentrack::{
        Client, ClientSettings, RetryPolicy, SyncStep,
        discovery::{self, VALIDATION_PREFIX_BYTES},
        ingest, log_url, store,
    },
    units::Timestamp,
};
use brewfixture::{Fault, FixtureServer, Kind, LogSpec, RangeMode, Scenario};

/// Retry off: nothing here tests the retry loop, and a wrong URL is wrong three times.
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

fn db() -> (Db, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let db = Db::open(dir.path().join("brewery.db")).expect("open the database");
    (db, dir)
}

async fn serve(spec: &LogSpec, kind: Kind) -> FixtureServer {
    FixtureServer::start(spec.remote_path(kind), spec.csv(kind)).await
}

// ---------------------------------------------------------------------------
// Probing
// ---------------------------------------------------------------------------

/// The happy path, under both `Range` behaviours. A proxy that strips `Range` answers
/// 200 with the whole file, and the probe must reach the same verdict either way —
/// otherwise validation would pass on nginx and fail behind a reverse proxy.
#[tokio::test]
async fn a_pasted_log_url_probes_to_its_header_and_ids() {
    let spec = Scenario::Gravity.spec();
    let server = serve(&spec, Kind::Full).await;

    for mode in [RangeMode::Honour, RangeMode::Ignore] {
        server.set_mode(mode);
        let probe = discovery::probe_log(&client(), "", &server.url())
            .await
            .unwrap_or_else(|err| panic!("{mode:?}: {err}"));

        assert_eq!(probe.url, server.url());
        assert_eq!(probe.base_url, server.base_url());
        assert_eq!(probe.remote_path, spec.remote_path(Kind::Full));
        assert_eq!(probe.file_name, spec.file_name(Kind::Full));
        assert_eq!(probe.device_id, Some(spec.device_id));
        assert_eq!(probe.log_id, Some(spec.log_id));
        assert_eq!(probe.suggested_name, spec.name);
        assert!(probe.header.gravity_enabled, "this scenario has a tilt");
        assert!(probe.header.header.starts_with("log_time,"));

        // The invariant the whole two-table split rests on.
        assert_eq!(
            log_url(&probe.base_url, &probe.remote_path).unwrap(),
            probe.url
        );
    }
}

/// A path relative to the configured base is the other way in — the user who knows
/// their box rather than the user with a browser tab open.
#[tokio::test]
async fn a_relative_path_resolves_against_the_configured_base_url() {
    let spec = Scenario::Nominal.spec();
    let server = serve(&spec, Kind::Full).await;

    let probe = discovery::probe_log(&client(), &server.base_url(), &spec.remote_path(Kind::Full))
        .await
        .expect("a full log");

    assert_eq!(probe.url, server.url());
    assert_eq!(probe.remote_path, spec.remote_path(Kind::Full));
}

/// The two mistakes 2f exists to tell apart. A `_graph.csv` is the right beer and the
/// wrong file; a gravity log is the wrong device entirely. One generic "not a log" for
/// both would leave the user guessing which.
#[tokio::test]
async fn the_graph_file_and_a_gravity_log_are_different_refusals() {
    let spec = Scenario::Gravity.spec();

    let server = serve(&spec, Kind::Graph).await;
    let err = discovery::probe_log(&client(), "", &server.url())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "fermentrack.graph_log");
    assert!(err.to_string().contains("_full.csv"), "{err}");
    assert!(err.to_string().contains(&spec.name), "{err}");

    // A `GravityLog`'s own file: `log_time` and `gravity`, none of the beer columns.
    let body = "log_time,gravity,grav_temp\r\n2025/09/18 00:00:00Z,1.048,19.4\r\n";
    let path = "/data/Gravity Device 2 - L5 - Tilt Red_full.csv";
    let server = FixtureServer::start(path, body).await;
    let err = discovery::probe_log(&client(), "", &server.url())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "fermentrack.gravity_log");
}

/// The three transport failures, each keeping the code the settings pane branches on.
/// A misdirected URL answering 200 with a dashboard is the one that would otherwise be
/// accepted, which is why the CSV check runs on the prefix too.
#[tokio::test]
async fn a_dashboard_a_missing_file_and_a_500_each_keep_their_code() {
    let spec = Scenario::Nominal.spec();
    let server = serve(&spec, Kind::Full).await;

    for (fault, code) in [
        (Fault::Html, "fermentrack.not_csv"),
        (Fault::NotFound, "fermentrack.log_missing"),
        (Fault::ServerError, "fermentrack.http_status"),
    ] {
        server.set_fault(fault);
        let err = discovery::probe_log(&client(), "", &server.url())
            .await
            .unwrap_err();
        assert_eq!(err.code(), code, "{fault:?}");
    }
}

/// The probe must read a bounded prefix of a log that may be megabytes. It asks with a
/// closed range and caps locally, so a `Range`-ignoring server costs one download and
/// not one per validation attempt.
#[tokio::test]
async fn the_probe_reads_a_bounded_prefix() {
    let spec = LogSpec {
        rows: 4000,
        ..Scenario::Nominal.spec()
    };
    let server = serve(&spec, Kind::Full).await;
    assert!(
        server.len() as u64 > VALIDATION_PREFIX_BYTES,
        "the fixture must be longer than the prefix for this to prove anything"
    );

    let bytes = client()
        .fetch_prefix(&server.url(), VALIDATION_PREFIX_BYTES)
        .await
        .expect("a prefix");

    assert_eq!(bytes.len() as u64, VALIDATION_PREFIX_BYTES);
    assert_eq!(
        server.last_range().as_deref(),
        Some("bytes=0-2047"),
        "a closed range, so nginx sends only what we need"
    );

    // And the same cap when the server ignores `Range` entirely.
    server.set_mode(RangeMode::Ignore);
    let bytes = client()
        .fetch_prefix(&server.url(), VALIDATION_PREFIX_BYTES)
        .await
        .expect("a prefix");
    assert_eq!(bytes.len() as u64, VALIDATION_PREFIX_BYTES);
}

/// A beer created seconds ago has a log file with nothing in it. That is a real state,
/// not a transport failure — but it is not something we can validate either, so it must
/// say so rather than being accepted as a source that never syncs.
#[tokio::test]
async fn an_empty_log_is_refused_by_name() {
    let server = FixtureServer::start("/data/Device 1 - B1 - New_full.csv", "").await;

    let err = discovery::probe_log(&client(), "", &server.url())
        .await
        .unwrap_err();

    assert_eq!(err.code(), "fermentrack.not_a_log");
    assert!(err.to_string().contains("empty"), "{err}");
}

// ---------------------------------------------------------------------------
// /api/devices/
// ---------------------------------------------------------------------------

/// The one call that answers "is this host a Fermentrack at all". It lists ids and
/// nothing else, which is exactly why paste-a-URL is the discovery story.
#[tokio::test]
async fn the_devices_endpoint_confirms_the_host() {
    let spec = Scenario::Nominal.spec();
    let server = serve(&spec, Kind::Full).await;
    server.serve_also("/api/devices/", "application/json", "[1, 2]");

    let devices = discovery::check_fermentrack(&client(), &server.base_url())
        .await
        .expect("a device list");
    assert_eq!(devices, vec![1, 2]);
}

/// A web server that is not Fermentrack answers something — a dashboard, a 404 page,
/// somebody else's JSON. "Not a Fermentrack" and "not a log" send the user to different
/// fields, so they are different errors.
#[tokio::test]
async fn a_host_that_is_not_a_fermentrack_says_so() {
    let spec = Scenario::Nominal.spec();
    let server = serve(&spec, Kind::Full).await;

    // No `/api/devices/` registered at all: a plain static file server. A 404 here is
    // "no such endpoint", not "the log was renamed".
    let err = discovery::check_fermentrack(&client(), &server.base_url())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "fermentrack.not_fermentrack");

    server.serve_also("/api/devices/", "text/html", "<html>Login</html>");
    let err = discovery::check_fermentrack(&client(), &server.base_url())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "fermentrack.not_fermentrack");
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Both rows or neither, and the offset starts at zero: the prefix we validated is
/// thrown away on purpose, so the first real sync reads the file from the beginning.
#[tokio::test]
async fn adding_a_source_writes_both_rows_and_starts_at_offset_zero() {
    let (db, _dir) = db();
    let spec = Scenario::Gravity.spec();
    let server = serve(&spec, Kind::Full).await;

    let probe = discovery::probe_log(&client(), "", &server.url())
        .await
        .expect("a full log");
    let id = discovery::add_source(&db, &probe, &probe.suggested_name).expect("add the source");

    let rows = db.read(tier1::list_sources).expect("list sources");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, SourceKind::FermentrackBeerLog);
    assert_eq!(rows[0].name, spec.name);

    let config: discovery::SourceConfig =
        serde_json::from_str(rows[0].config.as_deref().expect("config JSON")).expect("parse it");
    assert_eq!(config.base_url, server.base_url());
    assert_eq!(config.device_id, Some(spec.device_id));
    assert_eq!(config.log_id, Some(spec.log_id));

    let state = store::load_sync_state(&db, id)
        .expect("read sync_state")
        .expect("the row exists");
    assert_eq!(state.remote_path, spec.remote_path(Kind::Full));
    assert_eq!(state.byte_offset, 0);
    assert_eq!(state.header, None);

    // `config` carries no path: two copies would eventually disagree, and the one the
    // fetch does not read would be the one the UI shows.
    assert!(
        !rows[0].config.as_deref().unwrap().contains("remote_path"),
        "the path belongs to sync_state alone"
    );
}

/// Repointing a source at a different beer must reset the offset — byte 4 000 of one
/// log says nothing about byte 4 000 of another. Same path, and the offset is kept.
#[tokio::test]
async fn repointing_a_source_resets_the_offset_and_the_same_path_does_not() {
    let (db, _dir) = db();
    let spec = Scenario::Nominal.spec();
    let server = serve(&spec, Kind::Full).await;

    let probe = discovery::probe_log(&client(), "", &server.url())
        .await
        .expect("a full log");
    let id = discovery::add_source(&db, &probe, "Fermenter 1").expect("add the source");

    // Pretend a sync has happened.
    db.write(|conn| {
        conn.execute(
            "UPDATE sync_state SET byte_offset = 4000 WHERE source_id = ?",
            [id],
        )?;
        Ok(())
    })
    .expect("move the offset");

    store::upsert_sync_state(&db, id, &probe.remote_path).expect("same path");
    assert_eq!(
        store::load_sync_state(&db, id)
            .unwrap()
            .unwrap()
            .byte_offset,
        4000,
        "the same file must not be refetched from zero"
    );

    store::upsert_sync_state(&db, id, "/data/Device 1 - B18 - Another_full.csv").expect("repoint");
    assert_eq!(
        store::load_sync_state(&db, id)
            .unwrap()
            .unwrap()
            .byte_offset,
        0
    );
}

/// The point of the whole phase: a source added by pasting a URL is one the existing
/// sync path can drive, with no hand-written `create_source` anywhere.
#[tokio::test]
async fn a_probed_source_syncs_end_to_end() {
    let (db, _dir) = db();
    let spec = LogSpec {
        rows: 30,
        ..Scenario::Gravity.spec()
    };
    let server = serve(&spec, Kind::Full).await;

    let probe = discovery::probe_log(&client(), "", &server.url())
        .await
        .expect("a full log");
    let id = discovery::add_source(&db, &probe, &probe.suggested_name).expect("add the source");

    let mut state = store::load_sync_state(&db, id).unwrap().unwrap();
    let url = log_url(&probe.base_url, &state.remote_path).expect("rebuild the URL");
    let outcome = client()
        .fetch(&url, state.byte_offset)
        .await
        .expect("fetch");
    let step = ingest(&state, outcome).expect("ingest");
    assert!(matches!(step, SyncStep::Advance(_)));
    let report = store::commit(&db, &mut state, &step, Timestamp::now()).expect("commit");

    assert_eq!(
        report.rows_inserted,
        30 * 8,
        "every metric a tilt-equipped fermenter carries"
    );
    assert_eq!(state.byte_offset as usize, server.len());

    let summaries = discovery::source_summaries(&db).expect("summaries");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, id);
    assert_eq!(
        summaries[0].remote_path.as_deref(),
        Some(&*state.remote_path)
    );
    assert_eq!(summaries[0].byte_offset as usize, server.len());
    assert!(summaries[0].newest_reading.is_some());
    assert_eq!(summaries[0].last_error, None);
}

/// A source whose `sync_state` row is missing must still be listed — that is the whole
/// reason the join is a `LEFT JOIN`, and the broken one is the one you need to see.
#[test]
fn a_source_with_no_sync_state_is_still_listed() {
    let (db, _dir) = db();
    db.write(|conn| tier1::create_source(conn, SourceKind::Manual, "Hydrometer", None))
        .expect("create the source");

    let summaries = discovery::source_summaries(&db).expect("summaries");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].remote_path, None);
    assert_eq!(summaries[0].byte_offset, 0);
}
