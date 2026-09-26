use brewcore::{
    Error,
    brewd::{self, JournalProbe},
    calc::stability::{ExpectedFg, FlatSpan, Stability, flat_spans},
    config::{Config, ConfigStore},
    db::{
        Db, DbStatus, Metric, SourceKind,
        batch::{self, BatchRow},
        compare::{self, Comparison, RelRange, YAxis, YMode},
        dashboard::{self, BatchDashboard, DisplayGravity},
        health::DeviceHealth,
        home::{self, HomeBatch},
        reference::{self, VesselKind, VesselRow},
        series::{self, BatchSeries, SeriesRange},
        stability,
        tier1::{self, StreamRow},
        timeline::{self, BatchEventRow},
    },
    fermentrack::{self, Client, LogProbe, SourceSummary, discovery},
    journal::JournalEntry,
    sync::{ReadingSource, SourceStatus, SyncManager},
    units::{Litres, Sg},
};

use crate::error::{ApiError, ApiResult};

#[tauri::command]
pub async fn get_config(config: tauri::State<'_, ConfigStore>) -> ApiResult<Config> {
    Ok(config.get())
}

/// Whole-document replace, not a patch. The settings pane has four fields and already
/// holds the `Config` it got from [`get_config`]; a patch type of nested `Option`s
/// would be more code than the thing it configures. Last-write-wins is fine with one
/// window and no background writer — revisit if Phase 6's rule engine starts writing
/// thresholds.
///
/// Returns the *stored* value, which may differ from the argument (a trimmed URL, a
/// clamped interval). The frontend adopts the return value, so normalisation is
/// visible without a second round trip.
///
/// The write is one small `sync_all`'d file, so it runs inline rather than through
/// `spawn_blocking`.
#[tauri::command]
pub async fn set_config(
    config_store: tauri::State<'_, ConfigStore>,
    config: Config,
) -> ApiResult<Config> {
    Ok(config_store.set(config)?)
}

/// Async so Tauri runs it off the main thread: `rusqlite` is blocking, and even a
/// cheap status query should not stall the webview. Phase 2's sync path will want
/// `spawn_blocking` for the same reason.
#[tauri::command]
pub async fn db_status(db: tauri::State<'_, Db>) -> ApiResult<DbStatus> {
    Ok(db.status()?)
}

// ---------------------------------------------------------------------------
// Phase 2f: sources
// ---------------------------------------------------------------------------

/// Validate a pasted log URL without writing anything.
///
/// `input` is a whole URL or a path relative to the configured base. The error is the
/// point of this command as much as the success: `fermentrack.graph_log` and
/// `fermentrack.gravity_log` are two different mistakes and the pane says so.
#[tauri::command]
pub async fn probe_log(
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    input: String,
) -> ApiResult<LogProbe> {
    let base_url = config.get().fermentrack.base_url;
    Ok(discovery::probe_log(&client, &base_url, &input).await?)
}

/// Validate and then create the source, in that order.
///
/// Deliberately re-probes rather than taking a `LogProbe` from the frontend: a write
/// must not trust state the client has been holding, and the round trip costs 2 KB.
#[tauri::command]
pub async fn add_source(
    db: tauri::State<'_, Db>,
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    manager: tauri::State<'_, SyncManager>,
    input: String,
    name: Option<String>,
) -> ApiResult<SourceSummary> {
    let base_url = config.get().fermentrack.base_url;
    let probe = discovery::probe_log(&client, &base_url, &input).await?;

    let name = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| probe.suggested_name.clone());

    // Two small statements in one transaction; `spawn_blocking` would cost more than
    // it saves, for the same reason `db_status` runs inline.
    let id = discovery::add_source(&db, &probe, &name)?;

    // Phase 2g: poll it now rather than at the next launch. Without this, pasting a URL
    // produces a row and no readings until the app is restarted, which reads as
    // "adding a source does nothing".
    manager.spawn(Box::new(fermentrack::build_source(&db, &client, id)?));
    summary(&db, id)
}

fn summary(db: &Db, id: i64) -> ApiResult<SourceSummary> {
    discovery::source_summary(db, id)?.ok_or_else(|| ApiError {
        code: "sync.missing_state".to_owned(),
        message: format!("source {id} vanished immediately after being created"),
        detail: None,
    })
}

/// The poller for a source, by kind. `None` for a kind nothing polls: manual entry,
/// and Fermentrack gravity logs, which nothing reads yet.
pub(crate) fn build_source(
    db: &Db,
    client: &Client,
    config: &ConfigStore,
    id: i64,
    kind: SourceKind,
) -> brewcore::Result<Option<Box<dyn ReadingSource>>> {
    Ok(match kind {
        SourceKind::FermentrackBeerLog => {
            Some(Box::new(fermentrack::build_source(db, client, id)?))
        }
        SourceKind::BrewdJournal => Some(Box::new(brewd::build_source(db, client, config, id)?)),
        SourceKind::FermentrackGravityLog | SourceKind::Manual => None,
    })
}

#[tauri::command]
pub async fn list_sources(db: tauri::State<'_, Db>) -> ApiResult<Vec<SourceSummary>> {
    Ok(discovery::source_summaries(&db)?)
}

/// One call to `GET /api/devices/`, purely to tell "wrong host or port" from "right
/// box, wrong path". It returns device ids and nothing else — no beers, no names —
/// which is why the log URL still has to be pasted.
#[tauri::command]
pub async fn check_fermentrack(
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    base_url: Option<String>,
) -> ApiResult<Vec<i64>> {
    // The pane's unsaved field wins when it has one, so "Test" tests what is on screen
    // rather than what was last saved.
    let base_url = base_url
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| config.get().fermentrack.base_url);
    Ok(discovery::check_fermentrack(&client, &base_url).await?)
}

// ---------------------------------------------------------------------------
// Phase Ve: brewd journals
// ---------------------------------------------------------------------------

/// Every journal `brewd` has. Doubles as the settings pane's "Test connection": only
/// `brewd` answers `/journals/` with a journal list.
#[tauri::command]
pub async fn list_journals(
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    base_url: Option<String>,
) -> ApiResult<Vec<JournalEntry>> {
    // As `check_fermentrack`: the unsaved field wins, so "Test" tests what is on screen.
    let base_url = base_url
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| config.get().brewd.base_url);
    let base_url = brewd::discovery::base_url(&base_url)?;
    Ok(brewd::list_journals(&client, base_url).await?)
}

#[tauri::command]
pub async fn probe_journal(
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    journal: String,
) -> ApiResult<JournalProbe> {
    let base_url = config.get().brewd.base_url;
    let base_url = brewd::discovery::base_url(&base_url)?;
    Ok(brewd::probe_journal(&client, base_url, &journal).await?)
}

/// Probe, create, start polling. Re-probes for the reason [`add_source`] does.
#[tauri::command]
pub async fn add_journal_source(
    db: tauri::State<'_, Db>,
    client: tauri::State<'_, Client>,
    config: tauri::State<'_, ConfigStore>,
    manager: tauri::State<'_, SyncManager>,
    journal: String,
    name: Option<String>,
) -> ApiResult<SourceSummary> {
    let base_url = config.get().brewd.base_url;
    let base_url = brewd::discovery::base_url(&base_url)?;
    let probe = brewd::probe_journal(&client, base_url, &journal).await?;

    let name = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| probe.suggested_name.clone());
    let id = brewd::add_source(&db, &probe, &name)?;
    manager.spawn(Box::new(brewd::build_source(&db, &client, &config, id)?));
    summary(&db, id)
}

// ---------------------------------------------------------------------------
// Phase 2g: sync
// ---------------------------------------------------------------------------

/// Poll this source now instead of at its next tick.
///
/// Returns as soon as the request is queued. The result arrives as a `sync://status`
/// event like every other transition — holding a command open across a week of backlog
/// would freeze the button that started it.
#[tauri::command]
pub async fn resync(manager: tauri::State<'_, SyncManager>, source_id: i64) -> ApiResult<()> {
    Ok(manager.resync(source_id)?)
}

/// Delete every reading for this source and derive them all again from byte zero.
///
/// Destructive to Tier 1 only, which is derived data — that is why it can be a button
/// at all. The confirmation lives in the UI; by the time this is called the user has
/// already been told the row count.
#[tauri::command]
pub async fn rebuild(manager: tauri::State<'_, SyncManager>, source_id: i64) -> ApiResult<()> {
    Ok(manager.rebuild(source_id)?)
}

/// Every source's current sync state, for first paint.
///
/// A window can open mid-poll, before any event has been emitted into it; without this
/// the pane would render "never synced" for a source that is working fine.
#[tauri::command]
pub async fn sync_status(manager: tauri::State<'_, SyncManager>) -> ApiResult<Vec<SourceStatus>> {
    Ok(manager.statuses())
}

// ---------------------------------------------------------------------------
// Phase Ve: vessels and their default streams
// ---------------------------------------------------------------------------

/// A vessel with the streams a pitch into it binds.
#[derive(Debug, serde::Serialize)]
pub struct VesselView {
    #[serde(flatten)]
    pub vessel: VesselRow,
    pub default_streams: Vec<i64>,
}

#[tauri::command]
pub async fn list_vessels(db: tauri::State<'_, Db>) -> ApiResult<Vec<VesselView>> {
    Ok(db.read(|conn| {
        reference::list_vessels(conn, false)?
            .into_iter()
            .map(|vessel| {
                let default_streams = reference::list_vessel_streams(conn, vessel.id)?;
                Ok(VesselView {
                    vessel,
                    default_streams,
                })
            })
            .collect()
    })?)
}

#[tauri::command]
pub async fn create_vessel(
    db: tauri::State<'_, Db>,
    name: String,
    kind: VesselKind,
    capacity_l: Option<f64>,
) -> ApiResult<i64> {
    let capacity_l = capacity_l.map(Litres::new).transpose()?;
    Ok(db.write(|conn| reference::create_vessel(conn, name.trim(), kind, capacity_l, None))?)
}

#[tauri::command]
pub async fn list_streams(db: tauri::State<'_, Db>) -> ApiResult<Vec<StreamRow>> {
    Ok(db.read(tier1::list_streams)?)
}

/// Replace a vessel's default streams. Existing bindings are untouched; the next pitch
/// is what changes.
#[tauri::command]
pub async fn set_vessel_streams(
    db: tauri::State<'_, Db>,
    vessel_id: i64,
    stream_ids: Vec<i64>,
) -> ApiResult<()> {
    Ok(db.write(|conn| {
        let tx = conn.transaction()?;
        reference::set_vessel_streams(&tx, vessel_id, &stream_ids)?;
        tx.commit()?;
        Ok(())
    })?)
}

// ---------------------------------------------------------------------------
// Phase 4d: the batch curve
// ---------------------------------------------------------------------------

/// What the curve draws. `battery_v`, `rssi_dbm`, `tilt_deg` and `state` are device
/// health, not the ferment, and stay off it.
const CURVE_METRICS: [Metric; 7] = [
    Metric::Sg,
    Metric::TempC,
    Metric::FridgeTempC,
    Metric::RoomTempC,
    Metric::GravityTempC,
    Metric::SetpointC,
    Metric::Ph,
];

#[tauri::command]
pub async fn list_batches(
    db: tauri::State<'_, Db>,
    include_archived: bool,
) -> ApiResult<Vec<BatchRow>> {
    Ok(db.read(|conn| batch::list_batches(conn, include_archived))?)
}

#[tauri::command]
pub async fn get_batch(db: tauri::State<'_, Db>, batch_id: i64) -> ApiResult<BatchRow> {
    Ok(db.read(|conn| batch::get_batch(conn, batch_id))?)
}

#[derive(Debug, serde::Serialize)]
pub struct CurveView {
    #[serde(flatten)]
    pub series: BatchSeries,
    /// The gravity axis label: `ui.gravity_display` is applied here, since the fits
    /// live in `calc` and the frontend has no copy of them.
    pub gravity_unit: &'static str,
}

#[tauri::command]
pub async fn batch_series(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_id: i64,
    range: Option<SeriesRange>,
    max_points: usize,
) -> ApiResult<CurveView> {
    let display = config.get().ui.gravity_display;
    let series = db.read(|conn| {
        series::batch_series(conn, batch_id, &CURVE_METRICS, range, max_points)?
            .in_gravity_display(display)
    })?;
    Ok(CurveView {
        series,
        gravity_unit: display.unit(),
    })
}

#[tauri::command]
pub async fn list_batch_events(
    db: tauri::State<'_, Db>,
    batch_id: i64,
) -> ApiResult<Vec<BatchEventRow>> {
    Ok(db.read(|conn| timeline::list_batch_events(conn, batch_id))?)
}

/// 4c's detector with its history collapsed to spans: the history is one entry per
/// raw reading since pitch, which is the payload 4a exists to avoid.
#[derive(Debug, serde::Serialize)]
pub struct StabilityView {
    pub stream_id: Option<i64>,
    pub current: Stability,
    pub expected_fg: Option<ExpectedFg>,
    pub spans: Vec<FlatSpan>,
}

#[tauri::command]
pub async fn batch_stability(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_id: i64,
) -> ApiResult<StabilityView> {
    let params = config.get().stability.params();
    let s = db.read(|conn| stability::batch_stability(conn, batch_id, &params))?;
    Ok(StabilityView {
        stream_id: s.stream_id,
        current: s.current,
        expected_fg: s.expected_fg,
        spans: flat_spans(&s.history),
    })
}

// ---------------------------------------------------------------------------
// Phase 4e: the status card
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct DashboardView {
    #[serde(flatten)]
    pub dashboard: BatchDashboard,
    /// The card's gravities in `ui.gravity_display`; the SG ones stay beside them.
    pub display: DisplayGravity,
    pub gravity_unit: &'static str,
}

#[tauri::command]
pub async fn batch_dashboard(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_id: i64,
) -> ApiResult<DashboardView> {
    let config = config.get();
    let (params, health) = (config.stability.params(), config.health.params());
    let dashboard = db.read(|conn| dashboard::batch_dashboard(conn, batch_id, &params, &health))?;
    Ok(DashboardView {
        display: dashboard.gravity_in(config.ui.gravity_display),
        dashboard,
        gravity_unit: config.ui.gravity_display.unit(),
    })
}

/// 3e's "use this as the OG". SG, not the display unit: the card sends back the
/// candidate's canonical value.
#[tauri::command]
pub async fn set_batch_og(db: tauri::State<'_, Db>, batch_id: i64, og: f64) -> ApiResult<()> {
    let og = Sg::new(og)?;
    if !og.is_plausible() {
        return Err(Error::MeasurementImplausible {
            metric: Metric::Sg,
            value: og.get(),
        }
        .into());
    }
    Ok(db.write(|conn| batch::set_batch_measurements(conn, batch_id, None, Some(og), None))?)
}

// ---------------------------------------------------------------------------
// Phase 4f: home
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct HomeView {
    pub batches: Vec<HomeBatchView>,
    pub devices: Vec<DeviceHealth>,
    pub gravity_unit: &'static str,
}

#[derive(Debug, serde::Serialize)]
pub struct HomeBatchView {
    #[serde(flatten)]
    pub batch: HomeBatch,
    /// As [`DashboardView::display`].
    pub display: DisplayGravity,
}

#[tauri::command]
pub async fn home_summary(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
) -> ApiResult<HomeView> {
    let config = config.get();
    let (params, health) = (config.stability.params(), config.health.params());
    let summary = db.read(|conn| home::home_summary(conn, &params, &health))?;
    let display = config.ui.gravity_display;
    Ok(HomeView {
        batches: summary
            .batches
            .into_iter()
            .map(|batch| HomeBatchView {
                display: batch.dashboard.gravity_in(display),
                batch,
            })
            .collect(),
        devices: summary.devices,
        gravity_unit: display.unit(),
    })
}

// ---------------------------------------------------------------------------
// Phase 5b: the comparison overlay
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct CompareView {
    #[serde(flatten)]
    pub comparison: Comparison,
    /// The raw gravity axis's label. A fraction has no unit, so only batches left raw
    /// are converted.
    pub gravity_unit: &'static str,
}

#[tauri::command]
pub async fn compare_batches(
    db: tauri::State<'_, Db>,
    config: tauri::State<'_, ConfigStore>,
    batch_ids: Vec<i64>,
    y_mode: YMode,
    with_temp: bool,
    range: Option<RelRange>,
    max_points: usize,
) -> ApiResult<CompareView> {
    let display = config.get().ui.gravity_display;
    let mut comparison = db.read(|conn| {
        compare::compare_batches(conn, &batch_ids, y_mode, with_temp, range, max_points)
    })?;
    for b in &mut comparison.batches {
        if let YAxis::Raw { .. } = b.y {
            for s in &mut b.gravity {
                s.in_gravity_display(display)?;
            }
        }
    }
    Ok(CompareView {
        comparison,
        gravity_unit: display.unit(),
    })
}
