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
    assert_eq!(err["code"], "db.duplicate");

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

/// 5e. Canonical units whatever the display unit, in the configured dialect, at the
/// path the dialog chose.
#[test]
fn exports_write_the_screens_query_in_the_configured_dialect() {
    let app = Harness::new();
    let a = pitched(&app, "SELTZER-001", Some(1.040));
    let b = pitched(&app, "SELTZER-002", None);
    let mut config = app.invoke("get_config", json!({})).unwrap();
    config["ui"]["gravity_display"] = json!("plato");
    config["export"]["dialect"] = json!("excel_fr");
    app.invoke("set_config", json!({ "config": config }))
        .unwrap();
    let dir = app._dir.path();
    let read = |name: &str| {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        text.strip_prefix('\u{feff}').expect("a BOM").to_owned()
    };

    let path = dir.join("batches.csv");
    app.invoke(
        "export_query",
        json!({ "filter": filter(json!({})), "table": "batches", "path": path }),
    )
    .unwrap();
    let batches = read("batches.csv");
    let mut lines = batches.lines();
    assert!(lines.next().unwrap().starts_with("match;batch_id;code;"));
    assert!(
        lines.next().unwrap().starts_with(&format!(
            "matched;{a};SELTZER-001;seltzer;fermenting;1700000000;2023-11-14T22:13:20Z;;;1,04;"
        )),
        "{batches}"
    );

    let path = dir.join("strains.csv");
    app.invoke(
        "export_query",
        json!({ "filter": filter(json!({})), "table": "strains", "path": path }),
    )
    .unwrap();
    assert!(read("strains.csv").contains(&format!("no_yeast;;;2;;{a} | {b};")));

    let path = dir.join("overlay.csv");
    app.invoke(
        "export_comparison",
        json!({
            "batchIds": [a, b],
            "yMode": "fraction",
            "withTemp": false,
            "range": null,
            "path": path,
        }),
    )
    .unwrap();
    let overlay = read("overlay.csv");
    assert!(overlay.contains(";attenuation_fraction;"), "{overlay}");
    assert!(overlay.contains(";sg;"), "{overlay}");
    assert!(overlay.contains(";no_og;"), "{overlay}");

    let path = dir.join("readings.csv");
    app.invoke(
        "export_batch_readings",
        json!({ "batchId": a, "path": path }),
    )
    .unwrap();
    // A header and the fixture's 96 readings.
    assert_eq!(read("readings.csv").lines().count(), 97);

    let bad = dir.join("missing").join("out.csv");
    let err = app.api_error(
        "export_batch_readings",
        json!({ "batchId": a, "path": bad }),
    );
    assert_eq!(err["code"], "export.write");
}

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U1
// ---------------------------------------------------------------------------

/// Top-level arguments are camelCase (`includeArchived`); the fields inside
/// `ingredient` and `strain` are serde's, so snake_case.
#[test]
fn a_yeast_and_a_nutrient_are_created_and_listed() {
    let app = Harness::new();
    let strain = app
        .invoke(
            "create_yeast_strain",
            json!({ "strain": {
                "name": "EC-1118", "lab": "Lallemand", "form": "dry",
                "temp_min_c": 10.0, "temp_max_c": 30.0,
                "attenuation_min": 0.9, "attenuation_max": 1.0,
                "alcohol_tolerance": 0.18, "notes": null,
            }}),
        )
        .unwrap();

    let yeast = app
        .invoke(
            "create_ingredient",
            json!({ "ingredient": {
                "name": "EC-1118", "category": "yeast", "default_unit": "g",
                "yeast_strain_id": strain, "mg_yan_per_g": null, "notes": null,
            }}),
        )
        .unwrap();
    let dap = app
        .invoke(
            "create_ingredient",
            json!({ "ingredient": {
                "name": "DAP", "category": "nutrient", "default_unit": "g",
                "yeast_strain_id": null, "mg_yan_per_g": 212.0, "notes": null,
            }}),
        )
        .unwrap();

    let list = app
        .invoke("list_ingredients", json!({ "includeArchived": false }))
        .unwrap();
    let by_id = |id: &Value| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == *id)
            .unwrap()
            .clone()
    };
    assert_eq!(by_id(&yeast)["yeast_strain"], "EC-1118 (Lallemand)");
    assert_eq!(by_id(&dap)["mg_yan_per_g"], 212.0);
    assert_eq!(by_id(&dap)["lot_count"], 0);

    let presets = app.invoke("yan_presets", json!({})).unwrap();
    assert_eq!(presets[0], json!({ "label": "DAP", "mg_yan_per_g": 212.0 }));

    // The archived flag is the argument here that would silently default.
    app.invoke("archive_ingredient", json!({ "id": dap }))
        .unwrap();
    let active = app
        .invoke("list_ingredients", json!({ "includeArchived": false }))
        .unwrap();
    assert_eq!(active.as_array().unwrap().len(), 1);
    let all = app
        .invoke("list_ingredients", json!({ "includeArchived": true }))
        .unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert_eq!(
        app.invoke("restore_ingredient", json!({ "id": dap }))
            .unwrap(),
        true
    );
}

/// The form never sees `attrs`, so an edit must not drop a key it could not show.
#[test]
fn an_ingredient_edit_keeps_attrs_the_form_does_not_show() {
    let app = Harness::new();
    let id = app
        .invoke(
            "create_ingredient",
            json!({ "ingredient": {
                "name": "Fermaid O", "category": "nutrient", "default_unit": "g",
                "yeast_strain_id": null, "mg_yan_per_g": 160.0, "notes": null,
            }}),
        )
        .unwrap();
    let id = id.as_i64().unwrap();
    app.db
        .write(|conn| {
            conn.execute(
                "UPDATE ingredient SET attrs = json_set(attrs, '$.organic', 1) WHERE id = ?",
                [id],
            )?;
            Ok(())
        })
        .unwrap();

    app.invoke(
        "update_ingredient",
        json!({ "id": id, "ingredient": {
            "name": "Fermaid O", "category": "nutrient", "default_unit": "kg",
            "yeast_strain_id": null, "mg_yan_per_g": 40.0, "notes": "TOSNA figure",
        }}),
    )
    .unwrap();

    let attrs: String = app
        .db
        .read(|conn| {
            Ok(
                conn.query_row("SELECT attrs FROM ingredient WHERE id = ?", [id], |row| {
                    row.get(0)
                })?,
            )
        })
        .unwrap();
    let attrs: Value = serde_json::from_str(&attrs).unwrap();
    assert_eq!(attrs, json!({ "mg_yan_per_g": 40.0, "organic": 1 }));
}

#[test]
fn a_lot_cost_is_parsed_from_what_was_typed() {
    let app = Harness::new();
    let sucrose = app
        .invoke(
            "create_ingredient",
            json!({ "ingredient": {
                "name": "Sucrose", "category": "sugar", "default_unit": "kg",
                "yeast_strain_id": null, "mg_yan_per_g": null, "notes": null,
            }}),
        )
        .unwrap();
    let lot = |entry: Value, cost: Value, currency: Value| {
        json!({ "lot": {
            "ingredient_id": sucrose, "supplier": "Metro", "lot_code": null,
            "entry": entry, "cost": cost, "currency": currency,
            "purchased_at": 1_700_000_000, "expires_at": null, "notes": null,
        }})
    };
    let lots = |include_archived: bool| {
        app.invoke(
            "list_lots",
            json!({ "ingredientId": sucrose, "includeArchived": include_archived }),
        )
        .unwrap()
    };
    let five_kg = json!({ "value": 5.0, "unit": "kg" });

    let id = app
        .invoke(
            "create_lot",
            lot(five_kg.clone(), json!("12,50"), json!("eur")),
        )
        .unwrap();
    let listed = lots(false);
    assert_eq!(
        listed[0]["cost"],
        json!({ "minor": 1250, "currency": "EUR" })
    );
    assert_eq!(listed[0]["cost_per_canonical_unit"], 0.25);
    assert_eq!(listed[0]["entry"], five_kg);
    assert_eq!(listed[0]["batch_count"], 0);

    // A blank cost is no cost, whatever the currency box says.
    let mut edit = lot(
        json!({ "value": 2.5, "unit": "kg" }),
        json!(" "),
        json!("EUR"),
    );
    edit["id"] = id.clone();
    app.invoke("update_lot", edit).unwrap();
    let listed = lots(false);
    assert_eq!(listed[0]["cost"], Value::Null);
    assert_eq!(listed[0]["qty"], json!({ "mass": 2500.0 }));

    let err = app.api_error(
        "create_lot",
        lot(five_kg.clone(), json!("12,50"), Value::Null),
    );
    assert_eq!(err["code"], "money.invalid_currency");
    let err = app.api_error(
        "create_lot",
        lot(five_kg.clone(), json!("twelve"), json!("EUR")),
    );
    assert_eq!(err["code"], "money.invalid");
    let err = app.api_error(
        "create_lot",
        lot(
            json!({ "value": 5.0, "unit": "L" }),
            Value::Null,
            Value::Null,
        ),
    );
    assert_eq!(err["code"], "units.dimension_mismatch");

    app.invoke("archive_lot", json!({ "id": id })).unwrap();
    assert_eq!(lots(false), json!([]));
    assert_eq!(lots(true).as_array().unwrap().len(), 1);
}

#[test]
fn a_refused_reference_write_comes_back_with_a_code() {
    let app = Harness::new();
    let ingredient = |name: &str, category: &str, strain: Value| {
        json!({ "ingredient": {
            "name": name, "category": category, "default_unit": "g",
            "yeast_strain_id": strain, "mg_yan_per_g": null, "notes": null,
        }})
    };

    let err = app.api_error(
        "create_ingredient",
        ingredient("US-05", "yeast", Value::Null),
    );
    assert_eq!(err["code"], "db.yeast_needs_strain");

    app.invoke(
        "create_ingredient",
        ingredient("Sucrose", "sugar", Value::Null),
    )
    .unwrap();
    let err = app.api_error(
        "create_ingredient",
        ingredient("Sucrose", "sugar", Value::Null),
    );
    assert_eq!(err["code"], "db.duplicate");

    let err = app.api_error(
        "create_yeast_strain",
        json!({ "strain": { "name": "US-05", "attenuation_min": 78.0 } }),
    );
    assert_eq!(err["code"], "units.not_a_fraction");

    let err = app.api_error(
        "update_yeast_strain",
        json!({ "id": 404, "strain": { "name": "US-05" } }),
    );
    assert_eq!(err["code"], "db.no_such_yeast_strain");
}

// ---------------------------------------------------------------------------
// Phase 3 UI pass, U2
// ---------------------------------------------------------------------------

fn sugar(app: &Harness) -> Value {
    app.invoke(
        "create_ingredient",
        json!({ "ingredient": {
            "name": "Sucrose", "category": "sugar", "default_unit": "kg",
            "yeast_strain_id": null, "mg_yan_per_g": null, "notes": null,
        }}),
    )
    .unwrap()
}

fn item(ingredient: &Value, value: f64, unit: &str, stage: &str, hours: Value) -> Value {
    json!({
        "ingredient_id": ingredient, "entry": { "value": value, "unit": unit },
        "stage": stage, "offset_hours": hours, "notes": null,
    })
}

/// A recipe arrives with an empty v1; targets go in and come out in the display unit.
#[test]
fn a_recipe_is_created_with_v1_and_edited_in_the_display_unit() {
    let app = Harness::new();
    let created = app
        .invoke(
            "create_recipe",
            json!({ "recipe": { "name": " Citron ", "beverage_type": "seltzer", "notes": null } }),
        )
        .unwrap();
    let (recipe, v1) = (created["recipe_id"].clone(), created["version_id"].clone());

    let list = app
        .invoke("list_recipes", json!({ "includeArchived": false }))
        .unwrap();
    assert_eq!(list[0]["name"], "Citron");
    assert_eq!(list[0]["version_count"], 1);
    assert_eq!(list[0]["latest_version_no"], 1);

    let mut config = app.invoke("get_config", json!({})).unwrap();
    config["ui"]["gravity_display"] = json!("plato");
    app.invoke("set_config", json!({ "config": config }))
        .unwrap();

    app.invoke(
        "update_recipe_version",
        json!({ "id": v1, "version": {
            "target_volume_l": 20.0, "target_og": 10.0, "target_fg": 0.0,
            "target_abv": 0.055, "notes": "first go",
        }}),
    )
    .unwrap();
    let sucrose = sugar(&app);
    app.invoke(
        "add_recipe_item",
        json!({ "versionId": v1, "item": item(&sucrose, 4.0, "kg", "prep", Value::Null) }),
    )
    .unwrap();

    let view = app
        .invoke("get_recipe_version", json!({ "id": v1 }))
        .unwrap();
    assert_eq!(view["gravity_unit"], "°P");
    assert!(
        (view["target_og"].as_f64().unwrap() - 10.0).abs() < 1e-6,
        "{view}"
    );
    assert!(view["abv_alternate"].as_f64().unwrap() > 0.05, "{view}");
    assert_eq!(view["items"][0]["ingredient"], "Sucrose");
    assert_eq!(view["items"][0]["qty"], json!({ "mass": 4000.0 }));
    assert_eq!(view["batch_count"], 0);

    // Stored as SG regardless of the display.
    let og: f64 = app
        .db
        .read(|conn| {
            Ok(conn.query_row(
                "SELECT target_og FROM recipe_version WHERE id = ?",
                [v1.as_i64().unwrap()],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert!((og - 1.040).abs() < 0.001, "{og}");

    let detail = app.invoke("get_recipe", json!({ "id": recipe })).unwrap();
    assert_eq!(detail["recipe"]["name"], "Citron");
    assert_eq!(detail["versions"][0]["version_no"], 1);
}

/// The lock, the fork out of it, and a diff that reads a moved nutrient as one change.
#[test]
fn a_locked_version_forks_and_the_diff_pairs_a_moved_addition() {
    let app = Harness::new();
    let created = app
        .invoke(
            "create_recipe",
            json!({ "recipe": { "name": "Citron", "beverage_type": "seltzer", "notes": null } }),
        )
        .unwrap();
    let (recipe, v1) = (created["recipe_id"].clone(), created["version_id"].clone());
    let dap = app
        .invoke(
            "create_ingredient",
            json!({ "ingredient": {
                "name": "DAP", "category": "nutrient", "default_unit": "g",
                "yeast_strain_id": null, "mg_yan_per_g": 212.0, "notes": null,
            }}),
        )
        .unwrap();
    app.invoke(
        "add_recipe_item",
        json!({ "versionId": v1, "item": item(&dap, 2.0, "g", "fermentation", json!(24.0)) }),
    )
    .unwrap();

    app.db
        .write(|conn| {
            batch::create_batch(
                conn,
                &batch::NewBatch {
                    code: "SELTZER-001".into(),
                    recipe_version_id: v1.as_i64(),
                    beverage_type: BeverageType::Seltzer,
                    vessel_id: None,
                    volume_l: None,
                    og: None,
                    notes: None,
                },
                T0,
            )
        })
        .unwrap();

    let view = app
        .invoke("get_recipe_version", json!({ "id": v1 }))
        .unwrap();
    assert!(view["locked_at"].is_number());
    assert_eq!(view["batch_count"], 1);
    let line = view["items"][0]["id"].clone();
    let err = app.api_error(
        "update_recipe_item",
        json!({ "id": line, "item": item(&dap, 2.0, "g", "fermentation", json!(36.0)) }),
    );
    assert_eq!(err["code"], "db.recipe_version_locked");
    let err = app.api_error(
        "update_recipe",
        json!({ "id": recipe, "recipe": { "name": "Citron", "beverage_type": "cider", "notes": null } }),
    );
    assert_eq!(err["code"], "db.recipe_type_locked");

    let v2 = app
        .invoke("fork_recipe_version", json!({ "id": v1 }))
        .unwrap();
    assert_eq!(v2["version_no"], 2);
    let forked = app
        .invoke("get_recipe_version", json!({ "id": v2["id"] }))
        .unwrap();
    app.invoke(
        "update_recipe_item",
        json!({ "id": forked["items"][0]["id"], "item": item(&dap, 2.0, "g", "fermentation", json!(36.0)) }),
    )
    .unwrap();

    let diff = app
        .invoke(
            "diff_recipe_versions",
            json!({ "fromId": v1, "toId": v2["id"] }),
        )
        .unwrap();
    assert_eq!(diff["from_version_no"], 1);
    assert_eq!(diff["to_version_no"], 2);
    assert_eq!(diff["targets"], json!([]));
    let items = diff["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{diff}");
    assert_eq!(items[0]["change"], "changed");
    assert_eq!(items[0]["ingredient"], "DAP");
    assert_eq!(items[0]["from"]["offset_hours"], 24.0);
    assert_eq!(items[0]["to"]["offset_hours"], 36.0);

    let err = app.api_error("delete_recipe_version", json!({ "id": v1 }));
    assert_eq!(err["code"], "db.recipe_version_locked");
    app.invoke("delete_recipe_version", json!({ "id": v2["id"] }))
        .unwrap();
    let detail = app.invoke("get_recipe", json!({ "id": recipe })).unwrap();
    assert_eq!(detail["versions"].as_array().unwrap().len(), 1);
    assert_eq!(detail["versions"][0]["batch_count"], 1);
}

#[test]
fn a_refused_recipe_write_comes_back_with_a_code() {
    let app = Harness::new();
    let recipe = |name: &str| json!({ "recipe": { "name": name, "beverage_type": "seltzer", "notes": null } });
    let created = app.invoke("create_recipe", recipe("Citron")).unwrap();
    let v1 = created["version_id"].clone();

    assert_eq!(
        app.api_error("create_recipe", recipe("Citron"))["code"],
        "db.duplicate"
    );
    assert_eq!(
        app.api_error("create_recipe", recipe(" "))["code"],
        "db.name_empty"
    );

    let targets = |og: f64, fg: f64| {
        json!({ "id": v1, "version": {
            "target_volume_l": null, "target_og": og, "target_fg": fg,
            "target_abv": null, "notes": null,
        }})
    };
    assert_eq!(
        app.api_error("update_recipe_version", targets(2.5, 1.0))["code"],
        "db.measurement_implausible"
    );
    assert_eq!(
        app.api_error("update_recipe_version", targets(1.000, 1.040))["code"],
        "units.range_inverted"
    );

    let sucrose = sugar(&app);
    let err = app.api_error(
        "add_recipe_item",
        json!({ "versionId": v1, "item": item(&sucrose, 0.0, "kg", "prep", Value::Null) }),
    );
    assert_eq!(err["code"], "units.not_positive");
    let err = app.api_error(
        "add_recipe_item",
        json!({ "versionId": v1, "item": item(&sucrose, 1.0, "L", "prep", Value::Null) }),
    );
    assert_eq!(err["code"], "units.dimension_mismatch");
}

// ---------------------------------------------------------------------------
// U3: batches

fn batch_input(code: &str, version: Value, beverage_type: &str, og: Value) -> Value {
    json!({
        "code": code, "recipe_version_id": version, "beverage_type": beverage_type,
        "vessel_id": null, "volume_l": 20.0, "og": og, "notes": null,
    })
}

#[test]
fn a_batch_is_created_from_a_version_in_the_display_unit_and_locks_it() {
    let app = Harness::new();
    let created = app
        .invoke(
            "create_recipe",
            json!({ "recipe": { "name": "Citron", "beverage_type": "seltzer", "notes": null } }),
        )
        .unwrap();
    let v1 = created["version_id"].clone();

    let mut config = app.invoke("get_config", json!({})).unwrap();
    config["ui"]["gravity_display"] = json!("plato");
    app.invoke("set_config", json!({ "config": config }))
        .unwrap();

    let code = app
        .invoke("suggest_batch_code", json!({ "beverageType": "seltzer" }))
        .unwrap();
    assert_eq!(code, "SELTZER-001");
    let id = app
        .invoke(
            "create_batch",
            json!({ "batch": batch_input("SELTZER-001", v1.clone(), "seltzer", json!(10.0)) }),
        )
        .unwrap();

    let list = app
        .invoke("list_batches", json!({ "includeArchived": false }))
        .unwrap();
    assert_eq!(list[0]["id"], id);
    assert_eq!(list[0]["recipe_name"], "Citron");
    assert_eq!(list[0]["version_no"], 1);
    assert_eq!(list[0]["status"], "planned");
    // Stored as SG regardless of the display.
    assert!(
        (list[0]["og"].as_f64().unwrap() - 1.040).abs() < 0.001,
        "{list}"
    );

    let view = app
        .invoke("get_recipe_version", json!({ "id": v1 }))
        .unwrap();
    assert_eq!(view["batch_count"], 1);
    assert!(view["locked_at"].is_number(), "{view}");

    let refused = [
        (
            batch_input("SELTZER-001", Value::Null, "seltzer", Value::Null),
            "db.duplicate",
        ),
        (
            batch_input("BEER-001", v1, "beer", Value::Null),
            "db.beverage_type_mismatch",
        ),
        (
            batch_input("SELTZER-002", Value::Null, "seltzer", json!(250.0)),
            "calc.out_of_range",
        ),
    ];
    for (batch, code) in refused {
        assert_eq!(
            app.api_error("create_batch", json!({ "batch": batch }))["code"],
            code
        );
    }
}

#[test]
fn a_batch_is_pitched_moved_along_archived_and_restored() {
    let app = Harness::new();
    let id = app
        .invoke(
            "create_batch",
            json!({ "batch": batch_input("SELTZER-001", Value::Null, "seltzer", Value::Null) }),
        )
        .unwrap();
    let pitch = Timestamp::now().as_secs() - 86_400;
    let tomorrow = Timestamp::now().as_secs() + 86_400;

    let err = app.api_error(
        "record_pitch",
        json!({ "batchId": id, "at": tomorrow, "note": null }),
    );
    assert_eq!(err["code"], "db.in_future");

    app.invoke(
        "record_pitch",
        json!({ "batchId": id, "at": pitch, "note": "EC-1118" }),
    )
    .unwrap();
    let row = app.invoke("get_batch", json!({ "batchId": id })).unwrap();
    assert_eq!(row["pitched_at"], pitch);
    assert_eq!(row["status"], "fermenting");
    let err = app.api_error(
        "record_pitch",
        json!({ "batchId": id, "at": pitch, "note": null }),
    );
    assert_eq!(err["code"], "db.batch_already_pitched");

    assert_eq!(
        app.invoke("undo_pitch", json!({ "batchId": id })).unwrap(),
        true
    );
    app.invoke(
        "record_pitch",
        json!({ "batchId": id, "at": pitch, "note": null }),
    )
    .unwrap();

    let moved = app
        .invoke(
            "transition_batch",
            json!({ "batchId": id, "to": "conditioning", "at": pitch + 3_600 }),
        )
        .unwrap();
    assert_eq!(moved, true);
    let err = app.api_error("undo_pitch", json!({ "batchId": id }));
    assert_eq!(err["code"], "db.pitch_event_locked");
    let err = app.api_error(
        "transition_batch",
        json!({ "batchId": id, "to": "packaged", "at": pitch - 60 }),
    );
    assert_eq!(err["code"], "db.before_pitch");
    app.invoke(
        "transition_batch",
        json!({ "batchId": id, "to": "packaged", "at": null }),
    )
    .unwrap();

    app.invoke(
        "update_batch",
        json!({ "batchId": id, "batch": {
            "code": "SELTZER-001b", "vessel_id": null, "volume_l": 19.5, "notes": "dry",
        }}),
    )
    .unwrap();
    let row = app.invoke("get_batch", json!({ "batchId": id })).unwrap();
    assert_eq!(
        (row["code"].clone(), row["volume_l"].clone()),
        (json!("SELTZER-001b"), json!(19.5))
    );

    assert_eq!(
        app.invoke("archive_batch", json!({ "batchId": id }))
            .unwrap(),
        true
    );
    let active = app
        .invoke("list_batches", json!({ "includeArchived": false }))
        .unwrap();
    assert_eq!(active, json!([]));
    let err = app.api_error(
        "update_batch",
        json!({ "batchId": id, "batch": {
            "code": "SELTZER-001", "vessel_id": null, "volume_l": null, "notes": null,
        }}),
    );
    assert_eq!(err["code"], "db.batch_archived");
    assert_eq!(
        app.invoke("restore_batch", json!({ "batchId": id }))
            .unwrap(),
        true
    );
}

// ---------------------------------------------------------------------------
// U4: timeline and quick entry

fn pitched_by_ipc(app: &Harness) -> (Value, i64) {
    let id = app
        .invoke(
            "create_batch",
            json!({ "batch": batch_input("SELTZER-001", Value::Null, "seltzer", Value::Null) }),
        )
        .unwrap();
    let pitch = Timestamp::now().as_secs() - 86_400;
    app.invoke(
        "record_pitch",
        json!({ "batchId": id, "at": pitch, "note": null }),
    )
    .unwrap();
    (id, pitch)
}

#[test]
fn a_quick_entry_lands_on_the_timeline_in_the_display_unit() {
    let app = Harness::new();
    let (id, pitch) = pitched_by_ipc(&app);
    let mut config = app.invoke("get_config", json!({})).unwrap();
    config["ui"]["gravity_display"] = json!("plato");
    app.invoke("set_config", json!({ "config": config }))
        .unwrap();

    let entry = |ts: i64, measurements: Value, note: Value| json!({ "batchId": id, "entry": { "ts": ts, "measurements": measurements, "note": note } });
    let hydrometer = json!({
        "metric": "sg", "entry_value": 1.010, "entry_unit": "sg", "instrument": "hydrometer",
        "sample_temp_c": 25.0, "calibration_temp_c": 20.0,
    });
    let ph = json!({ "metric": "ph", "entry_value": 3.3, "entry_unit": "ph" });
    let result = app
        .invoke(
            "record_quick_entry",
            entry(pitch + 3_600, json!([hydrometer, ph]), json!("dry, lemony")),
        )
        .unwrap();
    assert!(result["event_id"].is_number(), "{result}");

    let view = app
        .invoke("batch_timeline", json!({ "batchId": id }))
        .unwrap();
    assert_eq!(view["gravity_unit"], "°P");
    assert_eq!(view["pitched_at"], pitch);
    let kinds: Vec<String> = view["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            format!(
                "{}:{}",
                i["type"].as_str().unwrap(),
                i["kind"].as_str().or(i["metric"].as_str()).unwrap()
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "event:pitch",
            "measurement:sg",
            "measurement:ph",
            "event:note"
        ]
    );
    let sg = &view["items"][1];
    // Temperature-corrected above the 1.010 typed, then shown in °P.
    assert!(sg["value"].as_f64().unwrap() > 1.010, "{sg}");
    assert!((sg["display"].as_f64().unwrap() - 2.8).abs() < 0.5, "{sg}");
    assert_eq!(view["items"][2]["display"], Value::Null);

    let refused = [
        (
            entry(
                pitch + 3_600,
                json!([{ "metric": "sg", "entry_value": 1.009, "entry_unit": "sg" }]),
                Value::Null,
            ),
            "db.measurement_exists",
        ),
        (
            entry(
                Timestamp::now().as_secs() + 86_400,
                json!([ph]),
                Value::Null,
            ),
            "db.in_future",
        ),
        (
            entry(
                pitch + 7_200,
                json!([{
                    "metric": "sg", "entry_value": 6.0, "entry_unit": "brix",
                    "instrument": "refractometer", "wort_correction": 1.0,
                }]),
                Value::Null,
            ),
            "db.no_original_gravity",
        ),
        (
            entry(
                pitch + 7_200,
                json!([{ "metric": "ph", "entry_value": 47.0, "entry_unit": "ph" }]),
                Value::Null,
            ),
            "db.measurement_implausible",
        ),
        (
            entry(pitch + 7_200, json!([]), json!(" ")),
            "db.entry_empty",
        ),
    ];
    for (args, code) in refused {
        assert_eq!(app.api_error("record_quick_entry", args)["code"], code);
    }

    let removed = app
        .invoke("delete_measurement", json!({ "measurementId": sg["id"] }))
        .unwrap();
    assert_eq!(removed, true);
    let view = app
        .invoke("batch_timeline", json!({ "batchId": id }))
        .unwrap();
    assert_eq!(view["items"].as_array().unwrap().len(), 3);
}

#[test]
fn events_are_added_corrected_and_deleted_and_og_fg_set_and_cleared() {
    let app = Harness::new();
    let (id, pitch) = pitched_by_ipc(&app);

    let event = app
        .invoke(
            "add_event",
            json!({ "batchId": id, "event": { "ts": pitch + 600, "kind": "cold_crash", "note": "2 °C" } }),
        )
        .unwrap();
    app.invoke(
        "update_event",
        json!({ "eventId": event, "event": { "ts": pitch + 1_200, "kind": "fining", "note": "gelatin" } }),
    )
    .unwrap();
    let events = app
        .invoke("list_batch_events", json!({ "batchId": id }))
        .unwrap();
    assert_eq!(events[1]["kind"], "fining");
    assert_eq!(events[1]["ts"], pitch + 1_200);
    let err = app.api_error(
        "add_event",
        json!({ "batchId": id, "event": { "ts": null, "kind": "pitch", "note": null } }),
    );
    assert_eq!(err["code"], "db.use_pitch_recorder");
    assert_eq!(
        app.invoke("delete_event", json!({ "eventId": event }))
            .unwrap(),
        true
    );

    app.invoke("set_batch_og", json!({ "batchId": id, "og": 1.040 }))
        .unwrap();
    app.invoke("set_batch_fg", json!({ "batchId": id, "fg": 0.998 }))
        .unwrap();
    let err = app.api_error("set_batch_fg", json!({ "batchId": id, "fg": 1.050 }));
    assert_eq!(err["code"], "units.range_inverted");
    let card = app
        .invoke("batch_dashboard", json!({ "batchId": id }))
        .unwrap();
    assert_eq!(card["fg"], 0.998);
    assert_eq!(card["display"]["fg"], 0.998);

    app.invoke("set_batch_og", json!({ "batchId": id, "og": null }))
        .unwrap();
    let row = app.invoke("get_batch", json!({ "batchId": id })).unwrap();
    assert_eq!(
        (row["og"].clone(), row["fg"].clone()),
        (Value::Null, json!(0.998))
    );
}
