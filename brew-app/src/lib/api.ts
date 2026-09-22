/**
 * The invoke boundary. Every command goes through here so error handling exists in
 * exactly one place and the Rust types are mirrored in exactly one file.
 */
import { invoke } from "@tauri-apps/api/core";
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
	sync: { poll_interval_secs: number };
	ui: { temp_display: TempDisplay; gravity_display: GravityDisplay };
}

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
	last_error: string | null;
	synced_at: number | null;
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
