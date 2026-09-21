//! Phase 2d's offset arithmetic against the mock Fermentrack.
//!
//! The rules themselves — what a half-written row costs, when an offset restarts — are
//! unit-tested inside `fermentrack::sync`, where they need no server. These are the
//! other half: the client, the parser and the offset agreeing about the same real bytes
//! over a real socket, with a file that grows between polls the way Fermentrack's does.
//!
//! No `Db` here. Phase 2e owns the transaction; 2d only decides what to put in it, so
//! the loop below holds `SyncState` in a local variable and that is the whole
//! persistence layer.

use brewcore::{
    fermentrack::{Client, ClientSettings, Restart, RestartReason, SyncState, SyncStep, ingest},
    units::Timestamp,
};
use brewfixture::{FixtureServer, Kind, LogSpec, RangeMode, Scenario};

fn client() -> Client {
    Client::with_settings(ClientSettings::default()).expect("build the client")
}

fn spec(rows: usize) -> LogSpec {
    LogSpec {
        rows,
        ..Scenario::Active.spec()
    }
}

fn state(remote_path: &str) -> SyncState {
    SyncState {
        source_id: 1,
        remote_path: remote_path.to_string(),
        byte_offset: 0,
        header: None,
        last_ts: None,
        last_error: None,
        synced_at: Timestamp::from_secs(0),
    }
}

/// One poll: fetch from wherever the state says, decide, fold the decision back in.
/// Phase 2g's scheduler is this plus a timer; Phase 2e's write path is this plus a
/// transaction.
async fn poll(client: &Client, url: &str, state: &mut SyncState) -> SyncStep {
    let outcome = client
        .fetch(url, state.byte_offset)
        .await
        .expect("the fixture server always answers");
    let step = ingest(state, outcome).expect("the fixture is a real _full.csv");
    step.apply_to(state, Timestamp::now());
    step
}

fn rows_of(step: &SyncStep) -> Vec<Timestamp> {
    match step {
        SyncStep::Advance(advance) => advance.rows.iter().map(|row| row.ts).collect(),
        _ => Vec::new(),
    }
}

/// The rows a longer run of the same log would have added, header stripped.
fn appended_rows(from: usize, to: usize) -> String {
    let longer = spec(to).full_csv();
    let shorter_rows = spec(from).full_csv().split_inclusive("\r\n").count();
    longer
        .split_inclusive("\r\n")
        .skip(shorter_rows)
        .collect::<String>()
}

/// Fermentrack's `writerow` is not atomic over the network: a poll can arrive with the
/// last line half on the wire. That row must be ingested exactly once, by the *next*
/// poll, and never twice.
#[tokio::test]
async fn a_row_split_across_two_polls_is_ingested_exactly_once() {
    let spec = spec(12);
    let body = spec.full_csv();
    // Mid-way through the last row.
    let cut = body.len() - 20;

    let server = FixtureServer::start(spec.remote_path(Kind::Full), &body.as_bytes()[..cut]).await;
    let url = server.url();
    let client = client();
    let mut state = state(spec.remote_path(Kind::Full).as_str());

    let mut seen = rows_of(&poll(&client, &url, &mut state).await);
    server.set_body(body.clone());
    seen.extend(rows_of(&poll(&client, &url, &mut state).await));

    let expected: Vec<Timestamp> = spec
        .generate()
        .iter()
        .map(|row| Timestamp::from_secs(row.ts))
        .collect();
    assert_eq!(seen, expected);
    assert_eq!(state.byte_offset, body.len() as u64);
    assert_eq!(state.last_error, None);
}

/// What makes dropping the remainder buffer safe: the offset sits on the last complete
/// record, so the next `Range` re-requests the partial tail for free.
#[tokio::test]
async fn the_next_poll_asks_for_the_partial_tail_again_without_a_remainder_buffer() {
    let spec = spec(12);
    let body = spec.full_csv();
    let cut = body.len() - 20;
    let complete = body[..cut].rfind("\r\n").expect("a complete record exists") + 2;

    let server = FixtureServer::start(spec.remote_path(Kind::Full), &body.as_bytes()[..cut]).await;
    let url = server.url();
    let client = client();
    let mut state = state(spec.remote_path(Kind::Full).as_str());

    poll(&client, &url, &mut state).await;
    assert_eq!(state.byte_offset, complete as u64);

    server.set_body(body);
    poll(&client, &url, &mut state).await;
    assert_eq!(server.last_range(), Some(format!("bytes={complete}-")));
}

#[tokio::test]
async fn a_log_that_grows_between_polls_never_repeats_a_row() {
    let spec = spec(20);
    let server = FixtureServer::start(spec.remote_path(Kind::Full), spec.full_csv()).await;
    let url = server.url();
    let client = client();
    let mut state = state(spec.remote_path(Kind::Full).as_str());

    let mut seen = rows_of(&poll(&client, &url, &mut state).await);
    server.append(appended_rows(20, 25));
    seen.extend(rows_of(&poll(&client, &url, &mut state).await));

    assert_eq!(seen.len(), 25);
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "{seen:?}");
    assert_eq!(state.byte_offset, server.len() as u64);
    assert_eq!(state.last_error, None);
}

/// A Range-ignoring proxy answers `200` with the whole body. The client skips the
/// offset locally, so 2d must not be able to tell the difference.
#[tokio::test]
async fn a_server_that_ignores_range_reaches_the_same_offset_as_one_that_honours_it() {
    let spec = spec(20);

    let mut reached = Vec::new();
    for mode in [RangeMode::Honour, RangeMode::Ignore] {
        let server = FixtureServer::start(spec.remote_path(Kind::Full), spec.full_csv()).await;
        server.set_mode(mode);
        let url = server.url();
        let client = client();
        let mut state = state(spec.remote_path(Kind::Full).as_str());

        let mut seen = rows_of(&poll(&client, &url, &mut state).await);
        server.append(appended_rows(20, 25));
        seen.extend(rows_of(&poll(&client, &url, &mut state).await));
        reached.push((seen, state.byte_offset));
    }

    assert_eq!(reached[0], reached[1]);
}

#[tokio::test]
async fn a_recreated_shorter_log_restarts_from_zero_and_re_reads_its_header() {
    let spec = spec(20);
    let server = FixtureServer::start(spec.remote_path(Kind::Full), spec.full_csv()).await;
    let url = server.url();
    let client = client();
    let mut state = state(spec.remote_path(Kind::Full).as_str());

    poll(&client, &url, &mut state).await;
    let old_header = state.header.clone().expect("a header was stored");

    // Same beer name, gravity sensor now gone: a shorter file with a narrower header.
    let recreated = LogSpec {
        rows: 3,
        gravity_enabled: false,
        ..spec.clone()
    };
    server.set_body(recreated.full_csv());

    let step = poll(&client, &url, &mut state).await;
    assert!(
        matches!(
            step,
            SyncStep::Restart(Restart {
                reason: RestartReason::Truncated { .. }
            })
        ),
        "{step:?}"
    );
    assert_eq!(state.byte_offset, 0);
    assert_eq!(state.header, None);
    assert_eq!(state.last_ts, None);

    // The refetch is what reads the new header — a resume never would have.
    let step = poll(&client, &url, &mut state).await;
    let SyncStep::Advance(advance) = &step else {
        panic!("expected the refetch to advance")
    };
    assert!(!advance.header_changed);
    assert_ne!(state.header.as_deref(), Some(old_header.as_str()));
    assert_eq!(state.byte_offset, server.len() as u64);
}

/// The case truncation cannot see. A log recreated at or past our offset leaves the
/// offset pointing into the middle of a record of a *different* file, and only the
/// record-boundary check catches it. This is why 2d needs no prefix probe.
#[tokio::test]
async fn a_log_recreated_at_the_same_length_is_caught_by_the_record_boundary_check() {
    let spec = spec(20);
    let server = FixtureServer::start(spec.remote_path(Kind::Full), spec.full_csv()).await;
    let url = server.url();
    let client = client();
    let mut state = state(spec.remote_path(Kind::Full).as_str());

    poll(&client, &url, &mut state).await;
    let offset = state.byte_offset;

    // A one-digit `associated_beer_id` makes every row a byte shorter, so the stored
    // offset lands inside a record of the new file rather than between two. A longer
    // body keeps us out of the truncation path entirely.
    let recreated = LogSpec {
        rows: 40,
        log_id: 5,
        ..spec.clone()
    };
    server.set_body(recreated.full_csv());
    assert!(server.len() as u64 > offset);

    let step = poll(&client, &url, &mut state).await;
    assert!(
        matches!(
            step,
            SyncStep::Restart(Restart {
                reason: RestartReason::NotOnARecordBoundary
            })
        ),
        "{step:?}"
    );
    assert_eq!(state.byte_offset, 0);

    // And the refetch recovers the whole file cleanly.
    poll(&client, &url, &mut state).await;
    assert_eq!(state.byte_offset, server.len() as u64);
}

#[tokio::test]
async fn an_idle_poll_writes_nothing_and_costs_one_request() {
    let spec = spec(12);
    let server = FixtureServer::start(spec.remote_path(Kind::Full), spec.full_csv()).await;
    let url = server.url();
    let client = client();
    let mut state = state(spec.remote_path(Kind::Full).as_str());

    poll(&client, &url, &mut state).await;
    let settled = state.clone();
    let before = server.request_count();

    let step = poll(&client, &url, &mut state).await;
    assert_eq!(step, SyncStep::Nothing);
    assert_eq!(server.request_count(), before + 1);
    assert_eq!(state.byte_offset, settled.byte_offset);
    assert_eq!(state.header, settled.header);
    assert_eq!(state.last_ts, settled.last_ts);
}

/// Every generated scenario, whole, in one pass — including the ugly one, whose
/// annotations carry a comma and a quote, whose `temp_format` switches mid-file, and
/// which has a multi-hour gap and absent sensors. All of it has to survive the split.
#[tokio::test]
async fn every_scenario_ingests_in_one_pass_with_no_skipped_rows() {
    for scenario in Scenario::ALL {
        let spec = scenario.spec();
        let body = spec.full_csv();
        let server = FixtureServer::start(spec.remote_path(Kind::Full), body.clone()).await;
        let mut state = state(spec.remote_path(Kind::Full).as_str());

        let step = poll(&client(), &server.url(), &mut state).await;
        let SyncStep::Advance(advance) = &step else {
            panic!("{}: expected an advance", scenario.name())
        };

        assert_eq!(advance.report.rows_parsed, spec.rows, "{}", scenario.name());
        assert_eq!(advance.report.rows_skipped, 0, "{}", scenario.name());
        assert_eq!(state.byte_offset, body.len() as u64, "{}", scenario.name());
        assert_eq!(state.last_error, None, "{}", scenario.name());

        let expected: Vec<Timestamp> = spec
            .generate()
            .iter()
            .map(|row| Timestamp::from_secs(row.ts))
            .collect();
        assert_eq!(rows_of(&step), expected, "{}", scenario.name());
    }
}
