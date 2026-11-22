/**
 * Phase 7a. Packaging lines, pure; `Packaging.svelte` and `BatchLifecycle.svelte` own the
 * calls. Volumes are litres, typed and shown. A missing volume is size × count, decided by
 * the backend; the form only shows it as the placeholder.
 */
import type { Packaging, PackageFormat, PackagingInput, PackagingLine } from "$lib/api";
import { parseNumber, reader, type Result } from "$lib/reference";
import { formatVolume } from "$lib/units";

export const FORMATS: { value: PackageFormat; label: string; size: string }[] = [
	{ value: "keg", label: "Keg", size: "19" },
	{ value: "bottle", label: "Bottle", size: "0.33" },
	{ value: "can", label: "Can", size: "0.33" },
	{ value: "other", label: "Other", size: "" },
];

const LABEL = Object.fromEntries(FORMATS.map((f) => [f.value, f.label])) as Record<PackageFormat, string>;

export interface PackagingForm {
	format: PackageFormat;
	/** Litres. */
	size: string;
	count: string;
	/** Litres; empty is size × count. */
	volume: string;
	notes: string;
}

const show = (v: number | null): string => (v === null ? "" : String(Number(v.toFixed(3))));

export function packagingForm(line: PackagingLine | null): PackagingForm {
	if (!line) return { format: "keg", size: "", count: "", volume: "", notes: "" };
	return {
		format: line.format,
		size: show(line.container_size_l),
		count: show(line.container_count),
		volume: show(line.volume_l),
		notes: line.notes ?? "",
	};
}

/** Size × count while the volume is empty, for its placeholder. */
export function impliedVolume(form: PackagingForm): number | null {
	const size = parseNumber(form.size);
	const count = parseNumber(form.count);
	if (size === null || count === null || Number.isNaN(size) || Number.isNaN(count)) return null;
	return Number((size * count).toFixed(3));
}

export function toPackagingInput(form: PackagingForm): Result<PackagingInput> {
	const r = reader();
	const count = r.num("count", form.count);
	if (count !== null && !Number.isInteger(count)) return { ok: false, error: "A count is a whole number" };
	return r.done({
		format: form.format,
		container_size_l: r.num("size", form.size),
		container_count: count,
		volume_l: r.num("volume", form.volume),
		notes: form.notes.trim() === "" ? null : form.notes.trim(),
	});
}

/** A container's size as it is sold: "330 mL", "19 L". */
export const sizeText = (litres: number): string =>
	litres < 1 ? formatVolume(litres) : `${Number(litres.toFixed(2))} L`;

/** "Keg · 1 × 19 L" or "Other · 4.50 L". */
export function lineText(line: PackagingLine): string {
	const containers =
		line.container_size_l !== null && line.container_count !== null
			? `${line.container_count} × ${sizeText(line.container_size_l)}`
			: formatVolume(line.volume_l);
	return `${LABEL[line.format]} · ${containers}`;
}

/** "2.70 L lost (13.5 %)", "none lost", or more packaged than the batch held. `null` with
 * nothing to say: no lines, or no batch volume. */
export function lossText(p: Packaging): string | null {
	if (p.loss_l === null || p.volume_l === null || p.volume_l <= 0) return null;
	const loss = p.loss_l;
	if (Math.abs(loss) < 0.005) return "none lost";
	if (loss < 0) return `${formatVolume(-loss)} more than the batch volume`;
	return `${formatVolume(loss)} lost (${((loss / p.volume_l) * 100).toFixed(1)} %)`;
}
