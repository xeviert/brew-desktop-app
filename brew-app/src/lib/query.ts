/**
 * Phase 5c. The filter form and the results table, pure; `QueryView.svelte` owns the
 * calls.
 *
 * The stored filter is canonical — SG, °C, epoch seconds — so a saved query means the
 * same thing after the display units change. The form holds what the user typed:
 * gravity in SG (the filter never converts it), temperature in the display unit, and
 * pitch dates as local days.
 */
import type { BatchFilter, Bounds, Computed, QueryRow, RollupGroup, Spread } from "$lib/api";
import { fromDisplayTemp, toDisplayTemp, type TempDisplay } from "$lib/units";

/**
 * Archived batches included: a saved query is usually a question about history, and
 * the batches it is about are the finished ones.
 */
export const emptyFilter = (): BatchFilter => ({
	beverage_types: [],
	statuses: [],
	include_archived: true,
	recipe_id: null,
	recipe_version_id: null,
	strain_ids: [],
	lot_ids: [],
	og: null,
	fg: null,
	ferm_temp_c: null,
	pitched: null,
});

type Picks = Omit<BatchFilter, "og" | "fg" | "ferm_temp_c" | "pitched">;

/** Range ends as typed; `""` is open. Dates are `yyyy-mm-dd`, as `<input type="date">`. */
export interface FilterForm extends Picks {
	og_min: string;
	og_max: string;
	fg_min: string;
	fg_max: string;
	temp_min: string;
	temp_max: string;
	pitched_from: string;
	pitched_to: string;
}

function dayStart(date: string): number | null {
	const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
	if (!m) return null;
	return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime() / 1000;
}

/** The next local midnight less a second: a DST day is 23 or 25 hours, not 24. */
function dayEnd(date: string): number | null {
	const start = dayStart(date);
	if (start === null) return null;
	const d = new Date(start * 1000);
	return new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1).getTime() / 1000 - 1;
}

function toDate(seconds: number): string {
	const d = new Date(seconds * 1000);
	const pad = (n: number) => String(n).padStart(2, "0");
	return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

const show = (v: number | null | undefined, f: (v: number) => number = (v) => v): string =>
	v === null || v === undefined ? "" : String(Number(f(v).toFixed(4)));

export function toForm(filter: BatchFilter, temp: TempDisplay): FilterForm {
	const t = (c: number) => toDisplayTemp(c, temp);
	const { og, fg, ferm_temp_c, pitched, ...picks } = filter;
	return {
		...picks,
		og_min: show(og?.min),
		og_max: show(og?.max),
		fg_min: show(fg?.min),
		fg_max: show(fg?.max),
		temp_min: show(ferm_temp_c?.min, t),
		temp_max: show(ferm_temp_c?.max, t),
		pitched_from: pitched?.min == null ? "" : toDate(pitched.min),
		// The stored end is the day's last second, so its local date is the day itself.
		pitched_to: pitched?.max == null ? "" : toDate(pitched.max),
	};
}

export type FormResult = { ok: true; filter: BatchFilter } | { ok: false; error: string };

/**
 * An unreadable end is an error, not an open end: quietly dropping it would widen the
 * result. `min > max` is left to the backend, which refuses it with a code.
 */
export function toFilter(form: FilterForm, temp: TempDisplay): FormResult {
	const { og_min, og_max, fg_min, fg_max, temp_min, temp_max, pitched_from, pitched_to, ...picks } =
		form;
	const bad: string[] = [];
	const num = (what: string, s: string, f: (v: number) => number = (v) => v): number | null => {
		if (s.trim() === "") return null;
		const v = Number(s);
		if (!Number.isFinite(v)) {
			bad.push(what);
			return null;
		}
		return f(v);
	};
	const date = (what: string, s: string, f: (s: string) => number | null): number | null => {
		if (s === "") return null;
		const v = f(s);
		if (v === null) bad.push(what);
		return v;
	};
	const bounds = (min: number | null, max: number | null): Bounds | null =>
		min === null && max === null ? null : { min, max };
	const c = (v: number) => fromDisplayTemp(v, temp);

	const filter: BatchFilter = {
		...picks,
		og: bounds(num("OG min", og_min), num("OG max", og_max)),
		fg: bounds(num("FG min", fg_min), num("FG max", fg_max)),
		ferm_temp_c: bounds(num("temp min", temp_min, c), num("temp max", temp_max, c)),
		pitched: bounds(
			date("pitched from", pitched_from, dayStart),
			date("pitched to", pitched_to, dayEnd),
		),
	};
	return bad.length ? { ok: false, error: `Not readable: ${bad.join(", ")}` } : { ok: true, filter };
}

export type SortKey =
	| "code"
	| "pitched_at"
	| "og"
	| "fg"
	| "lag"
	| "peak_rate"
	| "terminal"
	| "attenuation"
	| "abv"
	| "ferm_temp"
	| "yan"
	| "cost";

const val = <T, R>(c: Computed<T>, f: (v: T) => R): R | null => ("value" in c ? f(c.value) : null);

export function sortValue(row: QueryRow, key: SortKey): number | string | null {
	switch (key) {
		case "code":
			return row.code;
		case "pitched_at":
			return row.pitched_at;
		case "og":
			return row.og;
		case "fg":
			return row.fg;
		case "lag":
			return val(row.lag, (v) => v.secs);
		case "peak_rate":
			return val(row.peak_rate, (v) => v.sg_per_day);
		case "terminal":
			return val(row.terminal, (v) => v.secs_from_pitch);
		case "attenuation":
			return val(row.apparent_attenuation, (v) => v);
		case "abv":
			return val(row.abv_alternate, (v) => v);
		case "ferm_temp":
			return val(row.ferm_temp, (v) => v.median);
		case "yan":
			return val(row.yan_added_mg_per_l, (v) => v);
		case "cost":
			return val(row.cost_per_litre, (v) => v.minor);
	}
}

/** Missing values sort last either way: flipping the order shouldn't lead with blanks. */
export function sortRows(rows: QueryRow[], key: SortKey, desc: boolean): QueryRow[] {
	return [...rows].sort((a, b) => {
		const x = sortValue(a, key);
		const y = sortValue(b, key);
		if (x === null || y === null) {
			return x === y ? a.code.localeCompare(b.code) : x === null ? 1 : -1;
		}
		const cmp = typeof x === "string" ? x.localeCompare(y as string) : x - (y as number);
		return (desc ? -cmp : cmp) || a.code.localeCompare(b.code);
	});
}

/** 5b's limit. */
export const MAX_OVERLAY = 8;

/** A result small enough to overlay starts with every pitched batch ticked. */
export function preselect(rows: QueryRow[]): number[] {
	if (rows.length > MAX_OVERLAY) return [];
	return rows.filter((r) => r.pitched_at !== null).map((r) => r.batch_id);
}

/** 5d: `median (min–max)`, the range dropped when there's nothing to range over. */
export function formatSpread(s: Spread | null, f: (v: number) => string): string {
	if (s === null) return "—";
	return s.min === s.max ? f(s.median) : `${f(s.median)} (${f(s.min)}–${f(s.max)})`;
}

export function rollupLabel(group: RollupGroup): string {
	return group.kind === "strain"
		? group.name
		: `Mixed: ${group.strains.map((s) => s.name).join(" + ")}`;
}
