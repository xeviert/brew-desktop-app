/**
 * Phase 4d. `Curve` → uPlot's columnar data plus a description of each drawn series.
 * Pure, so the shaping is tested without a canvas; `Curve.svelte` owns everything
 * that touches uPlot itself.
 */
import type { Curve, Series } from "$lib/api";
import { toDisplayTemp, type TempDisplay } from "$lib/units";

/** Absolute time, or hours since pitch — Phase 5's overlay axis, built here so they agree. */
export type XMode = "abs" | "rel";

export type Axis = "grav" | "temp" | "ph";

const HOUR = 3_600;

/** One scale, since these are compared against each other. */
const TEMP_METRICS = new Set([
	"temp_c",
	"fridge_temp_c",
	"room_temp_c",
	"gravity_temp_c",
	"setpoint_c",
]);

export function axisOf(metric: string): Axis | null {
	if (metric === "sg") return "grav";
	if (metric === "ph") return "ph";
	return TEMP_METRICS.has(metric) ? "temp" : null;
}

export const COLORS: Record<string, string> = {
	sg: "#7ba7d4",
	temp_c: "#e0a35f",
	fridge_temp_c: "#d98ab4",
	room_temp_c: "#6f757d",
	gravity_temp_c: "#c7b26b",
	setpoint_c: "#9aa0a6",
	ph: "#7fb069",
};

/** Epoch seconds → the x value in `mode`. */
export function toX(ts: number, mode: XMode, pitchedAt: number | null): number {
	return mode === "rel" && pitchedAt !== null ? (ts - pitchedAt) / HOUR : ts;
}

/** The inverse of `toX`, for turning a zoom back into a range to fetch. */
export function toAbs(x: number, mode: XMode, pitchedAt: number | null): number {
	return mode === "rel" && pitchedAt !== null ? pitchedAt + x * HOUR : x;
}

/**
 * What a drawn column is. `smoothed` is the line and `raw` faint points behind it —
 * hiding raw would hide a smoothing bug. `lo`/`hi` are a bucket's extremes, drawn as a
 * band so the spike that says a probe fell out survives downsampling.
 */
export type Role = "line" | "smoothed" | "raw" | "manual" | "lo" | "hi";

export interface Drawn {
	streamId: number;
	metric: string;
	label: string;
	axis: Axis;
	role: Role;
	color: string;
}

export interface Plot {
	/** `[x, ...columns]`. `null` is a gap; `undefined` is only an alignment artefact. */
	data: (number | null | undefined)[][];
	/** One per column after x. */
	drawn: Drawn[];
	/** `[hi, lo]` column pairs, 1-based like uPlot's series indices. */
	bands: [number, number][];
	axes: Axis[];
}

/** Every series' own columns, keyed on its own timestamps. */
interface Table {
	ts: number[];
	cols: (number | null)[][];
}

/**
 * Put every table on one x column. A timestamp one series lacks becomes `undefined`,
 * which uPlot draws straight through; a `null` the server sent stays `null`, which it
 * breaks the line at. Same contract as `uPlot.join`'s default.
 */
export function align(tables: Table[]): (number | null | undefined)[][] {
	const xs = [...new Set(tables.flatMap((t) => t.ts))].sort((a, b) => a - b);
	const index = new Map(xs.map((x, i) => [x, i]));
	const out: (number | null | undefined)[][] = [xs];
	for (const t of tables) {
		for (const col of t.cols) {
			const aligned = new Array<number | null | undefined>(xs.length).fill(undefined);
			t.ts.forEach((ts, i) => (aligned[index.get(ts)!] = col[i]));
			out.push(aligned);
		}
	}
	return out;
}

function columns(s: Series): { role: Role; values: (number | null)[] }[] {
	if (s.source_kind === "manual") return [{ role: "manual", values: s.avg }];
	const band =
		s.bucket_secs !== null
			? [
					{ role: "hi" as const, values: s.max },
					{ role: "lo" as const, values: s.min },
				]
			: [];
	if (s.smoothed.length > 0) {
		return [{ role: "smoothed", values: s.smoothed }, { role: "raw", values: s.avg }, ...band];
	}
	return [{ role: "line", values: s.avg }, ...band];
}

export function buildPlot(
	curve: Curve,
	mode: XMode,
	pitchedAt: number | null,
	tempDisplay: TempDisplay,
): Plot {
	const tables: Table[] = [];
	const drawn: Drawn[] = [];
	const bands: [number, number][] = [];
	const axes = new Set<Axis>();

	for (const s of curve.series) {
		const axis = axisOf(s.metric);
		if (axis === null) continue;
		const convert =
			axis === "temp"
				? (v: number | null) => (v === null ? null : toDisplayTemp(v, tempDisplay))
				: (v: number | null) => v;

		const cols = columns(s);
		for (const { role } of cols) {
			drawn.push({
				streamId: s.stream_id,
				metric: s.metric,
				label: s.label,
				axis,
				role,
				color: COLORS[s.metric] ?? "#e6e6e6",
			});
		}
		const hi = cols.findIndex((c) => c.role === "hi");
		if (hi >= 0) bands.push([drawn.length - cols.length + hi + 1, drawn.length - cols.length + hi + 2]);
		tables.push({ ts: s.ts, cols: cols.map((c) => c.values.map(convert)) });

		// pH gets its axis only once there is a pH reading to put on it.
		if (axis !== "ph" || s.avg.some((v) => v !== null)) axes.add(axis);
	}

	const data = align(tables);
	data[0] = (data[0] as number[]).map((ts) => toX(ts, mode, pitchedAt));
	return {
		data,
		drawn,
		bands,
		axes: (["grav", "temp", "ph"] as const).filter((a) => axes.has(a)),
	};
}
