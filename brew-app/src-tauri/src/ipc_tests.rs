//! The command surface, invoked the way the webview invokes it.
//!
//! Tauri renames every snake_case command argument to camelCase, and a misspelt
//! `Option` argument does not fail — it arrives as `None` and the command quietly
//! falls back to the saved value. So these send the argument names `src/lib/api.ts`
//! sends, and each test is shaped so the wrong name would give a different answer.
//!
//! Nothing here reaches the network: every URL is empty or invalid.

use std::sync::Arc;

use brewcore::{
    config::ConfigStore,
    db::{Db, Metric, SourceKind, batch, migrations, recipe::BeverageType, tier1, timeline},
    fermentrack::Client,
    sync::{NoStatus, SyncManager},
    units::{Sg, Timestamp},
};
use serde_json::{Value, json};
use tauri::{
    WebviewWindow, WebviewWindowBuilder,
    ipc::{CallbackFn, InvokeBody},
    test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets},
    webview::InvokeRequest,
};

struct Harness {
    webview: WebviewWindow<MockRuntime>,
    db: Db,
    _app: tauri::App<MockRuntime>,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = ConfigStore::open_or_default(dir.path().join("config.toml"));
        let db = Db::open(dir.path().join("brewery.db")).unwrap();
        let manager = SyncManager::new(config.clone(), Arc::new(NoStatus));

        let app = mock_builder()
            .manage(config)
            .manage(db.clone())
            .manage(Client::new().unwrap())
            .manage(manager)
            .invoke_handler(super::handler())
            .build(mock_context(noop_assets()))
            .unwrap();
        let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();

        Self {
            webview,
            db,
            _app: app,
            _dir: dir,
        }
    }

    fn invoke(&self, cmd: &str, args: Value) -> Result<Value, Value> {
        get_ipc_response(
            &self.webview,
            InvokeRequest {
                cmd: cmd.into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: "http://tauri.localhost".parse().unwrap(),
                body: InvokeBody::Json(args),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.into(),
            },
        )
        .map(|body| body.deserialize().unwrap())
    }

    /// An error from our code, which the frontend reads `.code` off.
    fn api_error(&self, cmd: &str, args: Value) -> Value {
        let err = self.invoke(cmd, args).expect_err(cmd);
        assert!(err["code"].is_string(), "{cmd}: not an ApiError: {err}");
        err
    }
}

/// Every name `api.ts` calls is registered. An unregistered one fails with Tauri's own
/// message, which the first assertion pins so the check cannot pass vacuously.
#[test]
fn every_command_the_frontend_calls_is_registered() {
    let app = Harness::new();
    let unknown = app.invoke("no_such_command", json!({})).unwrap_err();
    assert!(unknown.to_string().contains("not found"), "{unknown}");

    let api = include_str!("../../src/lib/api.ts");
    let commands: Vec<&str> = api
        .split("call(\"")
        .skip(1)
        .map(|rest| &rest[..rest.find('"').unwrap()])
        .collect();
    assert!(commands.len() >= 17, "found only {commands:?}");

    for cmd in commands {
        // `baseUrl` keeps the two URL commands off the network; every other command
        // either ignores it or fails on a missing required argument first.
        if let Err(err) = app.invoke(cmd, json!({ "baseUrl": "not a url" })) {
            assert!(!err.to_string().contains("not found"), "{cmd}: {err}");
        }
    }
}

/// `toApiError` exists because of this: Tauri rejects bad arguments with a plain
/// string, before any of our code runs.
#[test]
fn a_missing_argument_is_rejected_by_tauri_as_a_string() {
    let app = Harness::new();
    let err = app.invoke("resync", json!({})).unwrap_err();
    assert!(err.is_string(), "{err}");
}

#[test]
fn config_round_trips_and_returns_the_stored_value() {
    let app = Harness::new();
    let mut config = app.invoke("get_config", json!({})).unwrap();
    config["sync"]["poll_interval_secs"] = json!(120);
    config["brewd"]["base_url"] = json!("  http://brewd.local:8080/  ");

    let stored = app
        .invoke("set_config", json!({ "config": config }))
        .unwrap();
    assert_eq!(stored["sync"]["poll_interval_secs"], 120);
    assert_ne!(
        stored["brewd"]["base_url"], config["brewd"]["base_url"],
        "set_config normalises, and returns what it stored"
    );
    assert_eq!(app.invoke("get_config", json!({})).unwrap(), stored);
}

#[test]
fn db_status_reports_the_schema_version() {
    let app = Harness::new();
    let status = app.invoke("db_status", json!({})).unwrap();
    assert_eq!(status["schema_version"], migrations::head());
    assert!(status["row_counts"]["reading"].is_number());
}

/// Both URL commands take an optional `baseUrl` that must win over the saved one. Sent
/// under the wrong name it would be `None`, and the error would name the saved (empty)
/// URL instead of this one.
#[test]
fn an_unsaved_base_url_is_the_one_tested() {
    let app = Harness::new();

    let err = app.api_error("check_fermentrack", json!({ "baseUrl": "arg-received" }));
    assert_eq!(err["code"], "fermentrack.url");
    assert!(
        err["message"].as_str().unwrap().contains("arg-received"),
        "{err}"
    );

    let err = app.api_error("list_journals", json!({ "baseUrl": "arg-received" }));
    assert_eq!(err["code"], "fermentrack.url");
    assert!(
        err["message"].as_str().unwrap().contains("arg-received"),
        "{err}"
    );

    let err = app.api_error("list_journals", json!({ "baseUrl": null }));
    assert_eq!(err["code"], "brewd.unconfigured");
}

#[test]
fn resync_and_rebuild_address_the_source_by_id() {
    let app = Harness::new();
    for cmd in ["resync", "rebuild"] {
        let err = app.api_error(cmd, json!({ "sourceId": 42 }));
        assert_eq!(err["code"], "sync.not_scheduled", "{cmd}");
    }
    assert_eq!(app.invoke("sync_status", json!({})).unwrap(), json!([]));
}

#[test]
fn a_vessel_is_created_and_given_default_streams() {
    let app = Harness::new();
    let stream = app
        .db
        .write(|conn| {
            let tx = conn.transaction()?;
            let source = tier1::create_source(&tx, SourceKind::BrewdJournal, "esp32-1", None)?;
            let stream = tier1::create_stream(&tx, source, "vessel_temp", Metric::TempC)?;
            tx.commit()?;
            Ok(stream)
        })
        .unwrap();

    let id = app
        .invoke(
            "create_vessel",
            json!({ "name": "  FV1 ", "kind": "fermenter", "capacityL": 30.0 }),
        )
        .unwrap();

    let streams = app.invoke("list_streams", json!({})).unwrap();
    assert_eq!(streams[0]["id"], stream);
    assert_eq!(streams[0]["metric"], "temp_c");

    app.invoke(
        "set_vessel_streams",
        json!({ "vesselId": id, "streamIds": [stream] }),
    )
    .unwrap();

    let vessels = app.invoke("list_vessels", json!({})).unwrap();
    assert_eq!(
        vessels,
        json!([{
            "id": id,
            "name": "FV1",
            "kind": "fermenter",
            "capacity_l": 30.0,
            "notes": null,
            "archived_at": null,
            "default_streams": [stream],
        }])
    );
}

/// The name is trimmed before the UNIQUE check, so padding does not make a second FV1.
#[test]
fn a_refused_vessel_write_comes_back_with_a_code() {
    let app = Harness::new();
    let id = app
        .invoke(
            "create_vessel",
            json!({ "name": "FV1", "kind": "fermenter", "capacityL": null }),
        )
        .unwrap();

    let err = app.api_error(
        "create_vessel",
        json!({ "name": " FV1 ", "kind": "keg", "capacityL": null }),
    );
    assert_eq!(err["code"], "db.sqlite");

    let err = app.api_error(
        "create_vessel",
        json!({ "name": "FV2", "kind": "fermenter", "capacityL": -5.0 }),
    );
    assert_eq!(err["code"], "units.not_positive");

    let err = app.api_error(
        "set_vessel_streams",
        json!({ "vesselId": id, "streamIds": [999] }),
    );
    assert_eq!(err["code"], "db.sqlite");

    let vessels = app.invoke("list_vessels", json!({})).unwrap();
    assert_eq!(vessels.as_array().unwrap().len(), 1);
    assert_eq!(vessels[0]["default_streams"], json!([]));
}

#[test]
fn only_kinds_with_a_poller_are_scheduled() {
    let app = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    let config = ConfigStore::open_or_default(dir.path().join("config.toml"));
    let client = Client::new().unwrap();

    for kind in [SourceKind::Manual, SourceKind::FermentrackGravityLog] {
        let built = super::commands::build_source(&app.db, &client, &config, 1, kind).unwrap();
        assert!(built.is_none(), "{kind:?}");
    }
    for kind in [SourceKind::FermentrackBeerLog, SourceKind::BrewdJournal] {
        let built = super::commands::build_source(&app.db, &client, &config, 999, kind);
        assert!(built.is_err(), "{kind:?}: a missing source must not build");
    }
}

/// A pitched batch with an iSpindle bound from the pitch: a day of readings at 15 min,
/// falling from 1.040.
fn pitched_batch(app: &Harness) -> i64 {
    pitched(app, "SELTZER-001", None)
}

/// A batch pitched at `T0` with a day of iSpindle gravity falling from 1.040.
fn pitched(app: &Harness, code: &str, og: Option<f64>) -> i64 {
    app.db
        .write(|conn| {
            let tx = conn.transaction()?;
            let batch_id = batch::create_batch(
                &tx,
                &batch::NewBatch {
                    code: code.into(),
                    recipe_version_id: None,
                    beverage_type: BeverageType::Seltzer,
                    vessel_id: None,
                    volume_l: None,
                    og: og.map(Sg::new).transpose()?,
                    notes: None,
                },
                T0,
            )?;
            let source = tier1::create_source(&tx, SourceKind::BrewdJournal, "ispindel", None)?;
            let stream = tier1::create_stream(&tx, source, "gravity", Metric::Sg)?;
            timeline::record_pitch(&tx, batch_id, T0, Some("EC-1118"))?;
            timeline::bind_stream(&tx, batch_id, stream, T0, None)?;
            let rows: Vec<_> = (0..96)
                .map(|i| (stream, at(i * 900), 1.040 - 0.0001 * i as f64))
                .collect();
            tier1::insert_readings(&tx, &rows)?;
            tx.commit()?;
            Ok(batch_id)
        })
        .unwrap()
}

const T0: Timestamp = Timestamp::from_secs(1_700_000_000);

fn at(offset_secs: i64) -> Timestamp {
    Timestamp::from_secs(T0.as_secs() + offset_secs)
}

#[test]
fn the_curve_commands_read_a_pitched_batch() {
    let app = Harness::new();
    let batch_id = pitched_batch(&app);

    let batches = app
        .invoke("list_batches", json!({ "includeArchived": false }))
        .unwrap();
    assert_eq!(batches[0]["id"], batch_id);
    let batch = app
        .invoke("get_batch", json!({ "batchId": batch_id }))
        .unwrap();
    assert_eq!(batch["pitched_at"], T0.as_secs());

    let events = app
        .invoke("list_batch_events", json!({ "batchId": batch_id }))
        .unwrap();
    assert_eq!(events[0]["kind"], "pitch");
    assert_eq!(events[0]["note"], "EC-1118");

    // A range under the right name clips; under a wrong one it would be the default,
    // pitch to now, and return every reading.
    let curve = app
        .invoke(
            "batch_series",
            json!({
                "batchId": batch_id,
                "range": { "start": T0.as_secs(), "end": at(3_600).as_secs() },
                "maxPoints": 1000,
            }),
        )
        .unwrap();
    assert_eq!(curve["gravity_unit"], "SG");
    assert_eq!(curve["range"]["end"], at(3_600).as_secs());
    let series = &curve["series"][0];
    assert_eq!(series["metric"], "sg");
    assert!(series["source_id"].is_number());
    assert_eq!(series["ts"].as_array().unwrap().len(), 4);
    assert_eq!(series["avg"][0], 1.040);

    let stability = app
        .invoke("batch_stability", json!({ "batchId": batch_id }))
        .unwrap();
    assert!(stability["spans"].is_array(), "{stability}");
    assert!(stability.get("history").is_none());
}

#[test]
fn the_gravity_axis_follows_the_display_setting() {
    let app = Harness::new();
    let batch_id = pitched_batch(&app);
    let mut config = app.invoke("get_config", json!({})).unwrap();
    config["ui"]["gravity_display"] = json!("plato");
    app.invoke("set_config", json!({ "config": config }))
        .unwrap();

    let curve = app
        .invoke(
            "batch_series",
            json!({ "batchId": batch_id, "range": null, "maxPoints": 1000 }),
        )
        .unwrap();
    assert_eq!(curve["gravity_unit"], "°P");
    let first = curve["series"][0]["avg"][0].as_f64().unwrap();
    assert!(
        (first - 10.0).abs() < 0.1,
        "1.040 is about 10 °P, got {first}"
    );
}

#[test]
fn a_refused_curve_comes_back_with_a_code() {
    let app = Harness::new();
    let batch_id = pitched_batch(&app);
    let err = app.api_error(
        "batch_series",
        json!({ "batchId": batch_id, "range": null, "maxPoints": 0 }),
    );
    assert_eq!(err["code"], "units.not_positive");
    for cmd in ["get_batch", "batch_stability"] {
        let err = app.api_error(cmd, json!({ "batchId": 404 }));
        assert_eq!(err["code"], "db.no_such_batch", "{cmd}");
    }
    let err = app.api_error(
        "batch_series",
        json!({ "batchId": 404, "range": null, "maxPoints": 1000 }),
    );
    assert_eq!(err["code"], "db.no_such_batch");
}

#[test]
fn two_batches_compare_as_a_fraction_or_raw_in_the_display_unit() {
    let app = Harness::new();
    let a = pitched(&app, "SELTZER-001", Some(1.040));
    let b = pitched(&app, "SELTZER-002", None);
    let mut config = app.invoke("get_config", json!({})).unwrap();
    config["ui"]["gravity_display"] = json!("plato");
    app.invoke("set_config", json!({ "config": config }))
        .unwrap();

    let view = app
        .invoke(
            "compare_batches",
            json!({
                "batchIds": [a, b],
                "yMode": "fraction",
                "withTemp": true,
                "range": { "start": 0, "end": 3_600 },
                "maxPoints": 1000,
            }),
        )
        .unwrap();
    assert_eq!(view["gravity_unit"], "°P");
    assert_eq!(view["span"]["end"], 3_600);
    let (a, b) = (&view["batches"][0], &view["batches"][1]);
    assert_eq!(a["y"], json!({ "kind": "fraction" }));
    assert_eq!(a["gravity"][0]["ts"].as_array().unwrap().len(), 4);
    assert_eq!(a["gravity"][0]["avg"][0], 0.0);
    assert!(a["temp"].is_null());
    // Left raw for want of an OG, and so converted like the curve.
    assert_eq!(b["y"], json!({ "kind": "raw", "reason": "no_og" }));
    let first = b["gravity"][0]["avg"][0].as_f64().unwrap();
    assert!(
        (first - 10.0).abs() < 0.1,
        "1.040 is about 10 °P, got {first}"
    );
}

#[test]
fn one_batch_is_not_a_comparison() {
    let app = Harness::new();
    let a = pitched_batch(&app);
    let err = app.api_error(
        "compare_batches",
        json!({
            "batchIds": [a, a],
            "yMode": "raw",
            "withTemp": false,
            "range": null,
            "maxPoints": 1000,
        }),
    );
    assert_eq!(err["code"], "compare.batch_count");
}

#[test]
fn the_card_offers_an_og_and_takes_it() {
    let app = Harness::new();
    let batch_id = pitched_batch(&app);
    let args = json!({ "batchId": batch_id });

    let card = app.invoke("batch_dashboard", args.clone()).unwrap();
    assert_eq!(card["og"], Value::Null);
    assert_eq!(card["estimate"]["state"], "no_og");
    assert_eq!(card["gravity_unit"], "SG");
    let candidate = card["og_candidate"]["sg"].as_f64().unwrap();
    assert_eq!(candidate, 1.040);
    assert_eq!(card["display"]["og_candidate"], candidate);

    app.invoke(
        "set_batch_og",
        json!({ "batchId": batch_id, "og": candidate }),
    )
    .unwrap();
    let card = app.invoke("batch_dashboard", args).unwrap();
    assert_eq!(card["og"], 1.040);
    assert_eq!(card["og_candidate"], Value::Null);
    assert_eq!(card["estimate"]["state"], "estimated");
    assert!(card["estimate"]["abv_alternate"].is_number(), "{card}");

    let err = app.api_error("set_batch_og", json!({ "batchId": batch_id, "og": 4.0 }));
    assert_eq!(err["code"], "db.measurement_implausible");
    let err = app.api_error("batch_dashboard", json!({ "batchId": 404 }));
    assert_eq!(err["code"], "db.no_such_batch");
}

#[test]
fn home_lists_the_fermenting_batch_and_its_devices() {
    let app = Harness::new();
    let batch_id = pitched_batch(&app);

    let home = app.invoke("home_summary", json!({})).unwrap();
    assert_eq!(home["gravity_unit"], "SG");
    let card = &home["batches"][0];
    assert_eq!(card["code"], "SELTZER-001");
    assert_eq!(card["dashboard"]["batch_id"], batch_id);
    assert!(card["display"]["gravity"].is_number(), "{card}");
    // The fixture's readings are from 2023: long silent.
    assert_eq!(card["streams"][0]["stale"], true);
    assert_eq!(home["devices"][0]["name"], "ispindel");
    assert_eq!(home["devices"][0]["stale"], true);
}

/// Every field, as `emptyFilter` in `src/lib/query.ts` sends it, with `over` on top.
fn filter(over: Value) -> Value {
    let mut filter = json!({
        "beverage_types": [],
        "statuses": [],
        "include_archived": true,
        "recipe_id": null,
        "recipe_version_id": null,
        "strain_ids": [],
        "lot_ids": [],
        "og": null,
        "fg": null,
        "ferm_temp_c": null,
        "pitched": null,
    });
    for (k, v) in over.as_object().unwrap() {
        filter[k] = v.clone();
    }
    filter
}

#[test]
fn a_query_is_saved_run_and_archived() {
    let app = Harness::new();
    let a = pitched(&app, "SELTZER-001", Some(1.040));
    let b = pitched(&app, "SELTZER-002", None);
    let mut config = app.invoke("get_config", json!({})).unwrap();
    config["ui"]["gravity_display"] = json!("plato");
    app.invoke("set_config", json!({ "config": config }))
        .unwrap();
    let og = filter(json!({ "og": { "min": 1.035, "max": 1.045 } }));

    let view = app
        .invoke("run_batch_query", json!({ "filter": og }))
        .unwrap();
    assert_eq!(view["gravity_unit"], "°P");
    assert_eq!(view["rows"][0]["batch_id"], a);
    let display_og = view["rows"][0]["display_og"].as_f64().unwrap();
    assert!((display_og - 10.0).abs() < 0.1, "got {display_og}");
    assert_eq!(
        view["unevaluated"],
        json!([{ "batch_id": b, "code": "SELTZER-002", "criterion": "og", "reason": "no_og" }])
    );
    // 5d rides along: no yeast recorded, so no strain row, but the batch is counted.
    assert_eq!(view["rollup"], json!({ "rows": [], "no_yeast": [a] }));

    let saved = app
        .invoke(
            "create_saved_query",
            json!({ "name": "Around 1.040", "filter": og }),
        )
        .unwrap();
    let id = saved["id"].as_i64().unwrap();
    assert_eq!(saved["filter"]["og"]["min"], 1.035);
    let list = app
        .invoke("list_saved_queries", json!({ "includeArchived": false }))
        .unwrap();
    assert_eq!(list[0]["name"], "Around 1.040");

    let everything = filter(json!({}));
    let updated = app
        .invoke(
            "update_saved_query",
            json!({ "id": id, "name": "Everything", "filter": everything }),
        )
        .unwrap();
    assert_eq!(updated["name"], "Everything");
    let run = app.invoke("run_saved_query", json!({ "id": id })).unwrap();
    assert_eq!(run["query"]["name"], "Everything");
    assert_eq!(run["result"]["rows"].as_array().unwrap().len(), 2);

    assert_eq!(
        app.invoke("archive_saved_query", json!({ "id": id }))
            .unwrap(),
        true
    );
    let archived = app
        .invoke("list_saved_queries", json!({ "includeArchived": true }))
        .unwrap();
    assert!(archived[0]["archived_at"].is_i64());
    let options = app.invoke("query_options", json!({})).unwrap();
    assert!(options["lots"].as_array().unwrap().is_empty());
}

#[test]
fn a_filter_the_backend_cannot_read_is_refused() {
    let app = Harness::new();
    let upside_down = filter(json!({ "fg": { "min": 1.010, "max": 1.000 } }));
    let err = app.api_error("run_batch_query", json!({ "filter": upside_down }));
    assert_eq!(err["code"], "query.invalid_filter");
    let err = app.api_error(
        "create_saved_query",
        json!({ "name": " ", "filter": filter(json!({})) }),
    );
    assert_eq!(err["code"], "query.name_empty");
}
