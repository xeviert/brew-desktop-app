//! Phase Vd end to end at the `journal` level: a `brewd` journal over a real socket into
//! a real SQLite file, with no `BrewdSource` in the way.
//!
//! `sync_write.rs` is the same set of promises against Fermentrack bytes. These are the
//! proof that they hold for the second dialect — LF terminators, integer times, cells
//! that are raw payloads — and that the gravity unit wedges rather than guesses.

use brewcore::{
    brewd::dialect::{self, JournalRow},
    config::IspindelUnit,
    db::{Db, Metric, SourceKind, tier1},
    journal::{Client, ClientSettings, FetchOutcome, SyncState, SyncStep, log_url, store},
    units::Timestamp,
};
use brewfixture::{FixtureServer, JournalScenario, JournalSpec, RangeMode};

struct Rig {
    db: Db,
    _dir: tempfile::TempDir,
    source_id: i64,
}

impl Rig {
    fn new(remote_path: &str) -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let db = Db::open(dir.path().join("brewery.db")).expect("open the database");
        let source_id = db
            .write(|conn| tier1::create_source(conn, SourceKind::BrewdJournal, "esp32-1", None))
            .expect("create the source");
        store::upsert_sync_state(&db, source_id, remote_path).expect("create sync_state");
        Self {
            db,
            _dir: dir,
            source_id,
        }
    }

    fn state(&self) -> SyncState {
        store::load_sync_state(&self.db, self.source_id)
            .expect("read sync_state")
            .expect("the row exists")
    }

    fn readings(&self) -> Vec<(Metric, Timestamp, f64)> {
        self.db
            .read(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT s.metric, r.ts, r.value FROM reading r \
                     JOIN stream s ON s.id = r.stream_id \
                     WHERE s.source_id = ? ORDER BY s.metric, r.ts",
                )?;
                let rows = stmt.query_map([self.source_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            })
            .expect("read the readings back")
    }

    /// Decide and write one outcome, as a poll would.
    fn apply(
        &self,
        unit: Option<IspindelUnit>,
        outcome: FetchOutcome,
    ) -> brewcore::Result<SyncStep<JournalRow>> {
        let mut state = self.state();
        let step = dialect::ingest(unit, &state, outcome)?;
        dialect::commit(&self.db, &mut state, &step, Timestamp::now())?;
        Ok(step)
    }
}

fn client() -> Client {
    Client::with_settings(ClientSettings::default()).expect("build the client")
}

async fn poll(rig: &Rig, client: &Client, url: &str, unit: Option<IspindelUnit>) {
    let outcome = client
        .fetch(url, rig.state().byte_offset)
        .await
        .expect("the fixture server always answers");
    rig.apply(unit, outcome)
        .expect("a generated journal parses");
}

/// Serve most of a journal ending mid-record, sync, serve the rest, sync. No gaps, no
/// duplicates, offset at the end. Returns the readings so the two modes can be compared.
async fn growing_journal(mode: RangeMode) -> Vec<(Metric, Timestamp, f64)> {
    let spec = JournalSpec {
        rows: 40,
        ..JournalScenario::Esp32.spec()
    };
    let body = spec.csv().into_bytes();
    let first_len = body.len() * 2 / 3;
    assert_ne!(body[first_len - 1], b'\n', "the cut must land mid-record");

    let server = FixtureServer::start(spec.remote_path(), body[..first_len].to_vec()).await;
    server.set_mode(mode);
    let rig = Rig::new(&spec.remote_path());
    let client = client();
    let url = log_url(&server.base_url(), &spec.remote_path()).expect("a valid URL");

    poll(&rig, &client, &url, None).await;
    let after_first = rig.readings();
    assert!(!after_first.is_empty());
    assert!(rig.state().byte_offset < first_len as u64);

    server.append(&body[first_len..]);
    poll(&rig, &client, &url, None).await;
    assert_eq!(rig.state().byte_offset, body.len() as u64);
    assert_eq!(
        rig.state().header.as_deref(),
        Some("time,vessel_temp,chamber_temp")
    );

    let all = rig.readings();
    for metric in [Metric::TempC, Metric::FridgeTempC] {
        let times: Vec<_> = all.iter().filter(|r| r.0 == metric).map(|r| r.1).collect();
        let expected: Vec<_> = (0..40)
            .map(|i| Timestamp::from_secs(spec.time_at(i)))
            .collect();
        assert_eq!(times, expected, "{metric:?}");
    }
    for earlier in &after_first {
        assert!(all.contains(earlier), "the second poll lost {earlier:?}");
    }
    all
}

#[tokio::test]
async fn a_growing_journal_lands_once_whether_or_not_range_is_honoured() {
    let honoured = growing_journal(RangeMode::Honour).await;
    let ignored = growing_journal(RangeMode::Ignore).await;
    assert_eq!(honoured, ignored);
}

/// Every place a poll can land, including inside a quoted cell and inside the
/// half-written tail. Each split must yield exactly the rows one whole read does.
/// Pure `ingest`, so the few hundred cuts cost no database.
#[test]
fn every_split_point_yields_what_one_read_does() {
    let spec = JournalScenario::Ugly.spec();
    let body = spec.csv().into_bytes();
    let blank = SyncState {
        source_id: 1,
        remote_path: spec.remote_path(),
        byte_offset: 0,
        header: None,
        last_ts: None,
        last_error: None,
        synced_at: Timestamp::from_secs(0),
    };
    let rows_of = |step: SyncStep<JournalRow>| match step {
        SyncStep::Advance(advance) => advance.rows,
        SyncStep::Nothing => Vec::new(),
        SyncStep::Restart(restart) => panic!("unexpected restart: {:?}", restart.reason),
    };

    let expected = rows_of(dialect::ingest(None, &blank, fresh(&body, 0)).unwrap());
    assert_eq!(expected.len(), spec.rows);

    for cut in 1..body.len() {
        let mut state = blank.clone();
        let first = dialect::ingest(None, &state, fresh(&body[..cut], 0)).unwrap();
        first.apply_to(&mut state, Timestamp::from_secs(0));
        let from = state.byte_offset as usize;
        let second = dialect::ingest(None, &state, fresh(&body[from..], from as u64)).unwrap();

        let mut rows = rows_of(first);
        rows.extend(rows_of(second));
        assert_eq!(rows, expected, "split at byte {cut}");
    }
}

/// `brewd` killed mid-write trims the half line on its next start, then appends. The
/// offset never moved past the half line, so nothing is lost and nothing is doubled.
#[tokio::test]
async fn a_half_line_trimmed_by_a_restart_is_neither_lost_nor_doubled() {
    let spec = JournalScenario::Ugly.spec();
    let body = spec.csv();
    let whole_lines = body.rfind('\n').expect("a newline") + 1;

    let server = FixtureServer::start(spec.remote_path(), body.clone()).await;
    let rig = Rig::new(&spec.remote_path());
    let client = client();
    let url = server.url();

    poll(&rig, &client, &url, None).await;
    assert_eq!(rig.state().byte_offset, whole_lines as u64);
    let before = rig.readings();

    // The trim, then the record written again in full, then one more.
    let records = spec.records();
    server.truncate_to(whole_lines);
    let tail = &records[spec.rows];
    server.append(format!(
        "{}\n{},18.4,17.2\n",
        tail.join(","),
        spec.time_at(spec.rows + 1)
    ));
    poll(&rig, &client, &url, None).await;

    let after = rig.readings();
    for earlier in &before {
        assert!(after.contains(earlier));
    }
    let temp_times: Vec<_> = after
        .iter()
        .filter(|r| r.0 == Metric::TempC)
        .map(|r| r.1)
        .collect();
    let mut deduped = temp_times.clone();
    deduped.dedup();
    assert_eq!(temp_times, deduped);
    assert_eq!(
        temp_times.last(),
        Some(&Timestamp::from_secs(spec.time_at(spec.rows + 1)))
    );
    assert_eq!(rig.state().last_error, None);
}

/// No unit, a gravity column: the poll fails and consumes nothing. Setting the unit
/// afterwards ingests every row the refusal held back, converted.
#[test]
fn a_gravity_journal_waits_for_its_unit() {
    let spec = JournalSpec {
        rows: 6,
        ..JournalScenario::Ispindel.spec()
    };
    let body = spec.csv().into_bytes();
    let rig = Rig::new(&spec.remote_path());

    let err = rig.apply(None, fresh(&body, 0)).unwrap_err();
    assert_eq!(err.code(), "brewd.ispindel_unit_unset");
    assert_eq!(rig.state().byte_offset, 0);
    assert!(rig.readings().is_empty());

    rig.apply(Some(IspindelUnit::Sg), fresh(&body, 0))
        .expect("parses once the unit is known");
    let sg: Vec<_> = rig
        .readings()
        .into_iter()
        .filter(|r| r.0 == Metric::Sg)
        .collect();
    assert_eq!(sg.len(), 6);
    assert_eq!(rig.state().byte_offset, body.len() as u64);
}

/// The probe is said to measure room air partway through. Later rows land on the same
/// stream under its new metric; no second `temp_c` stream appears for the column.
#[test]
fn a_moved_sensor_keeps_its_readings_and_gets_the_new_ones() {
    let spec = JournalSpec {
        rows: 10,
        ..JournalScenario::Esp32.spec()
    };
    let body = spec.csv().into_bytes();
    let half = body[..body.len() / 2]
        .iter()
        .rposition(|&b| b == b'\n')
        .expect("a newline")
        + 1;
    let rig = Rig::new(&spec.remote_path());

    rig.apply(None, fresh(&body[..half], 0)).expect("parses");
    let probe = rig
        .db
        .read(|conn| tier1::stream_ids_for(conn, rig.source_id))
        .unwrap()[&Metric::TempC];
    rig.db
        .write(|conn| {
            let tx = conn.transaction()?;
            tier1::set_stream_metric(&tx, probe, Metric::RoomTempC)?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    rig.apply(None, fresh(&body[half..], half as u64)).expect("parses");

    let all = rig.readings();
    assert!(all.iter().all(|r| r.0 != Metric::TempC));
    assert_eq!(all.iter().filter(|r| r.0 == Metric::RoomTempC).count(), 10);
    let streams = rig
        .db
        .read(|conn| tier1::stream_ids_for(conn, rig.source_id))
        .unwrap();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[&Metric::RoomTempC], probe);
}

fn fresh(bytes: &[u8], offset: u64) -> FetchOutcome {
    FetchOutcome::Fresh {
        bytes: bytes.to_vec(),
        offset,
        total: Some(offset + bytes.len() as u64),
    }
}
