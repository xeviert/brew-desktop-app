/**
 * Phase 3 UI pass, U3. The batch create and lifecycle forms, pure; `BatchNew.svelte` and
 * `BatchLifecycle.svelte` own the calls.
 *
 * OG is typed in `ui.gravity_display` and converted by the backend (4d's rule). Times are
 * a `datetime-local` value, read as local time. Range rules — a plausible OG, a positive
 * volume, a pitch not in the future — are the backend's, refused with a code.
 */
import type { BatchEdit, BatchInput, BatchStatus, BatchSummary, BeverageType } from "$lib/api";
import { reader, type Result } from "$lib/reference";
import type { GravityDisplay } from "$lib/units";

/** Mirrors `brewcore::units::GravityDisplay::unit`, for a field label. */
export const GRAVITY_UNIT: Record<GravityDisplay, string> = { sg: "SG", plato: "°P", brix: "°Bx" };

export const STATUSES: BatchStatus[] = ["planned", "fermenting", "conditioning", "packaged"];

/** The one move forward from each status that a button offers, if any. Leaving `planned`
 * is the pitch, which has its own form. */
export const NEXT: Partial<Record<BatchStatus, { to: BatchStatus; label: string }>> = {
	fermenting: { to: "conditioning", label: "Start conditioning" },
	conditioning: { to: "packaged", label: "Mark packaged" },
};

const pad = (n: number) => String(n).padStart(2, "0");

/** Epoch seconds → a `datetime-local` value in local time, to the minute. */
export function localDateTime(seconds: number): string {
	const d = new Date(seconds * 1000);
	return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** A `datetime-local` value → epoch seconds, or `null` if it is not one. */
export function fromLocalDateTime(s: string): number | null {
	const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2}))?$/.exec(s);
	if (!m) return null;
	const [y, mo, d, h, mi, sec] = m.slice(1).map((v) => Number(v ?? 0));
	return new Date(y, mo - 1, d, h, mi, sec).getTime() / 1000;
}

const text = (s: string): string | null => (s.trim() === "" ? null : s.trim());

// ------------------------------------------------------------------ create

export interface BatchForm {
	code: string;
	recipe_version_id: number | null;
	beverage_type: BeverageType;
	vessel_id: number | null;
	volume: string;
	/** In the display gravity unit. */
	og: string;
	notes: string;
}

export function toBatchInput(form: BatchForm): Result<BatchInput> {
	if (form.code.trim() === "") return { ok: false, error: "A batch needs a code" };
	const r = reader();
	return r.done({
		code: form.code.trim(),
		recipe_version_id: form.recipe_version_id,
		beverage_type: form.beverage_type,
		vessel_id: form.vessel_id,
		volume_l: r.num("volume", form.volume),
		og: r.num("OG", form.og),
		notes: text(form.notes),
	});
}

// -------------------------------------------------------------------- edit

export interface EditForm {
	code: string;
	vessel_id: number | null;
	volume: string;
	notes: string;
}

export function editForm(batch: BatchSummary): EditForm {
	return {
		code: batch.code,
		vessel_id: batch.vessel_id,
		volume: batch.volume_l === null ? "" : String(Number(batch.volume_l.toFixed(2))),
		notes: batch.notes ?? "",
	};
}

export function toBatchEdit(form: EditForm): Result<BatchEdit> {
	if (form.code.trim() === "") return { ok: false, error: "A batch needs a code" };
	const r = reader();
	return r.done({
		code: form.code.trim(),
		vessel_id: form.vessel_id,
		volume_l: r.num("volume", form.volume),
		notes: text(form.notes),
	});
}

// -------------------------------------------------------------------- list

export function recipeLabel(b: BatchSummary): string {
	return b.recipe_name === null ? "no recipe" : `${b.recipe_name} v${b.version_no}`;
}

/** Batches grouped by status, in process order, empty groups left out. */
export function byStatus<T extends { status: BatchStatus }>(batches: T[]): { status: BatchStatus; items: T[] }[] {
	return STATUSES.map((status) => ({ status, items: batches.filter((b) => b.status === status) })).filter(
		(g) => g.items.length > 0,
	);
}
