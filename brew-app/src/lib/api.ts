/**
 * The invoke boundary. Every command goes through here so error handling exists in
 * exactly one place and the Rust types are mirrored in exactly one file.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { GravityDisplay, TempDisplay } from "$lib/units";

/** Mirrors `src-tauri/src/error.rs`. `code` is stable; `message` is for humans. */
export interface ApiError {
	code: string;
	message: string;
	detail?: string | null;
}

/** Mirrors `brewcore::config::Config`. Field names are serde's, so snake_case. */
export interface Config {
	fermentrack: { base_url: string };
	brewd: { base_url: string };
	sync: { poll_interval_secs: number };
	ui: { temp_display: TempDisplay; gravity_display: GravityDisplay };
	measurement: { hydrometer_calibration_c: number; refractometer_wort_correction: number };
	gravity: { ispindel_unit: IspindelUnit | null };
}

/** Mirrors `brewcore::config::IspindelUnit`. `null` in `Config` means not recorded yet. */
export type IspindelUnit = "sg" | "plato";

/** Mirrors `brewcore::db::DbStatus`. */
export interface DbStatus {
	path: string;
	size_bytes: number;
	schema_version: number;
	row_counts: Record<string, number>;
}

/** Mirrors `brewcore::fermentrack::csv_log::HeaderInfo`. */
export interface HeaderInfo {
	header: string;
	gravity_enabled: boolean;
	unknown_columns: string[];
}

/** Mirrors `brewcore::fermentrack::discovery::LogProbe`. */
export interface LogProbe {
	url: string;
	base_url: string;
	remote_path: string;
	file_name: string;
	suggested_name: string;
	device_id: number | null;
	log_id: number | null;
	header: HeaderInfo;
}

/** Mirrors `brewcore::fermentrack::discovery::SourceSummary`. Timestamps are epoch seconds. */
export interface SourceSummary {
	id: number;
	kind: string;
	name: string;
	remote_path: string | null;
	byte_offset: number;
	last_ts: number | null;
	newest_reading: number | null;
	/** Readings this source owns. Named in Rebuild's confirmation. */
	reading_count: number;
	last_error: string | null;
	synced_at: number | null;
}

/** Mirrors `brewcore::journal::JournalEntry`, one row of `brewd`'s `GET /journals/`. */
export interface JournalEntry {
	name: string;
	bytes: number;
	modified: number;
}

/** Mirrors `brewcore::brewd::MappedColumn`. `metric` is `Metric::as_str`. */
export interface MappedColumn {
	column: string;
	metric: string;
}

/** Mirrors `brewcore::brewd::JournalHeader`. */
export interface JournalHeader {
	header: string;
	mapped: MappedColumn[];
	unknown_columns: string[];
	/** A `gravity` column: the journal will not sync until the iSpindle unit is set. */
	needs_ispindel_unit: boolean;
}

/** Mirrors `brewcore::brewd::JournalProbe`. */
export interface JournalProbe {
	journal: string;
	remote_path: string;
	suggested_name: string;
	/** Renamed away by `brewd` on a header change. It will not grow. */
	rotated: boolean;
	header: JournalHeader;
}

/**
 * Tauri rejects with whatever the command returned, so ours arrive as `ApiError` —
 * but argument-deserialisation failures and panics do not. Tauri produces those
 * itself, as plain strings, before our code runs, so anything reading `e.code`
 * straight off a rejection eventually renders `undefined`.
 */
export function toApiError(e: unknown): ApiError {
	if (typeof e === "object" && e !== null && "code" in e && "message" in e) {
		return e as ApiError;
	}
	return { code: "unknown", message: typeof e === "string" ? e : String(e) };
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return await invoke<T>(cmd, args);
	} catch (e) {
		throw toApiError(e);
	}
}

export const getConfig = (): Promise<Config> => call("get_config");

/** Returns the *stored* config, which may differ from `config` after normalisation. */
export const setConfig = (config: Config): Promise<Config> => call("set_config", { config });

export const dbStatus = (): Promise<DbStatus> => call("db_status");

/**
 * Validate a pasted log URL or path without writing anything. Rejects with a code the
 * caller is meant to branch on: `fermentrack.graph_log` and `fermentrack.gravity_log`
 * are two different mistakes.
 */
export const probeLog = (input: string): Promise<LogProbe> => call("probe_log", { input });

/** Validates again server-side before writing; the probe is not taken on trust. */
export const addSource = (input: string, name?: string): Promise<SourceSummary> =>
	call("add_source", { input, name: name ?? null });

export const listSources = (): Promise<SourceSummary[]> => call("list_sources");

/** Device ids from `GET /api/devices/`, purely to confirm the host is a Fermentrack. */
export const checkFermentrack = (baseUrl?: string): Promise<number[]> =>
	call("check_fermentrack", { baseUrl: baseUrl ?? null });

// ---------------------------------------------------------------------------
// Phase Ve: brewd
// ---------------------------------------------------------------------------

/** Every journal `brewd` has. Also the brewd "Test connection": an unsaved URL wins. */
export const listJournals = (baseUrl?: string): Promise<JournalEntry[]> =>
	call("list_journals", { baseUrl: baseUrl ?? null });

/** Read a journal's header against the saved brewd URL. Writes nothing. */
export const probeJournal = (journal: string): Promise<JournalProbe> =>
	call("probe_journal", { journal });

/** Probes again server-side, adds, and starts polling. */
export const addJournalSource = (journal: string, name?: string): Promise<SourceSummary> =>
	call("add_journal_source", { journal, name: name ?? null });

// ---------------------------------------------------------------------------
// Phase 2g: sync
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::sync::SyncPhase`. */
export type SyncPhase = "idle" | "syncing" | "error" | "unreachable";

/** Mirrors `brewcore::sync::SourceStatus`. Timestamps are epoch seconds. */
export interface SourceStatus {
	source_id: number;
	phase: SyncPhase;
	last_success: number | null;
	last_error: string | null;
	/** `ApiError.code` of the last failed poll; `null` for a committed fault. */
	last_error_code: string | null;
	rows_added: number;
	newest_reading: number | null;
	consecutive_failures: number;
	next_poll_at: number | null;
}

/**
 * Poll now instead of at the next tick. Resolves as soon as the request is queued —
 * the result arrives on `sync://status`, not here.
 */
export const resync = (sourceId: number): Promise<void> => call("resync", { sourceId });

/**
 * Delete every reading for this source and derive them again from byte zero.
 * Destructive to derived data only, but confirm with the user before calling it.
 */
export const rebuild = (sourceId: number): Promise<void> => call("rebuild", { sourceId });

/** Every source's current state, for first paint before any event has arrived. */
export const syncStatus = (): Promise<SourceStatus[]> => call("sync_status");

/**
 * Subscribe to sync transitions. Returns the unlisten function — call it on destroy,
 * or a re-mounted component gets two of every event.
 *
 * `listen` resolves asynchronously, so an unsubscribe that races the subscribe has to
 * be remembered rather than ignored; hence the flag.
 */
export function onSyncStatus(cb: (status: SourceStatus) => void): () => void {
	let stop: (() => void) | null = null;
	let cancelled = false;

	void listen<SourceStatus>("sync://status", (event) => cb(event.payload)).then((unlisten) => {
		if (cancelled) unlisten();
		else stop = unlisten;
	});

	return () => {
		cancelled = true;
		stop?.();
	};
}

// ---------------------------------------------------------------------------
// Phase Ve: vessels
// ---------------------------------------------------------------------------

/** Mirrors `brewcore::db::reference::VesselKind`. */
export type VesselKind = "fermenter" | "brite" | "keg" | "carboy" | "kettle" | "other";

/** Mirrors `src-tauri`'s `VesselView`: a `VesselRow` plus its default stream ids. */
export interface Vessel {
	id: number;
	name: string;
	kind: VesselKind;
	capacity_l: number | null;
	notes: string | null;
	archived_at: number | null;
	/** Bound to a batch from its pitch, closed when it is packaged. */
	default_streams: number[];
}

/** Mirrors `brewcore::db::tier1::StreamRow`. */
export interface Stream {
	id: number;
	source_id: number;
	source_name: string;
	source_kind: string;
	external_id: string;
	metric: string;
}

export const listVessels = (): Promise<Vessel[]> => call("list_vessels");

export const createVessel = (
	name: string,
	kind: VesselKind,
	capacityL: number | null,
): Promise<number> => call("create_vessel", { name, kind, capacityL });

export const listStreams = (): Promise<Stream[]> => call("list_streams");

/** Replaces the whole set. Batches already pitched keep what they were bound to. */
export const setVesselStreams = (vesselId: number, streamIds: number[]): Promise<void> =>
	call("set_vessel_streams", { vesselId, streamIds });
