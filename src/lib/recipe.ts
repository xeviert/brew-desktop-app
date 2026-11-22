/**
 * Phase 3 UI pass, U2. The recipe screen's forms and tables, pure; `RecipeView.svelte`
 * owns the calls.
 *
 * Target gravities are typed and shown in `ui.gravity_display` and converted by the
 * backend (4d's rule: no gravity fit in TS). ABV is typed in percent and sent as a
 * fraction. Range rules — plausible gravity, FG ≤ OG, a positive quantity — are the
 * backend's, refused with a code; here only "not a number" is caught.
 */
import type {
	BeverageType,
	ItemChange,
	RecipeItem,
	RecipeItemInput,
	RecipeVersion,
	RecipeVersionInput,
	Stage,
	TargetChange,
	Unit,
} from "$lib/api";
import { formatQuantity, parseNumber, type Result } from "$lib/reference";

export const BEVERAGE_TYPES: BeverageType[] = ["seltzer", "beer", "cider", "wine", "mead", "kombucha", "other"];

/** Process order, which is also how a recipe reads. */
export const STAGES: Stage[] = ["prep", "pitch", "fermentation", "conditioning", "packaging"];

const show = (v: number | null, f: (v: number) => number = (v) => v, digits = 4): string =>
	v === null ? "" : String(Number(f(v).toFixed(digits)));

/** Collects unreadable fields by label, as `lib/reference.ts` does. */
function reader() {
	const bad: string[] = [];
	return {
		num(label: string, s: string, f: (v: number) => number = (v) => v): number | null {
			const v = parseNumber(s);
			if (v === null) return null;
			if (Number.isNaN(v)) {
				bad.push(label);
				return null;
			}
			return f(v);
		},
		done<T>(value: T): Result<T> {
			return bad.length ? { ok: false, error: `Not readable: ${bad.join(", ")}` } : { ok: true, value };
		},
	};
}

// ----------------------------------------------------------------- targets

export interface TargetsForm {
	volume: string;
	/** In the display gravity unit. */
	og: string;
	fg: string;
	/** Percent. */
	abv: string;
	notes: string;
}

export function targetsForm(version: RecipeVersion): TargetsForm {
	return {
		volume: show(version.target_volume_l, undefined, 2),
		og: show(version.target_og),
		fg: show(version.target_fg),
		abv: show(version.target_abv, (f) => f * 100, 2),
		notes: version.notes ?? "",
	};
}

export function toVersionInput(form: TargetsForm): Result<RecipeVersionInput> {
	const r = reader();
	return r.done({
		target_volume_l: r.num("volume", form.volume),
		target_og: r.num("OG", form.og),
		target_fg: r.num("FG", form.fg),
		target_abv: r.num("ABV", form.abv, (v) => v / 100),
		notes: form.notes.trim() === "" ? null : form.notes.trim(),
	});
}

// ------------------------------------------------------------------- items

export interface ItemForm {
	ingredient_id: number | null;
	qty: string;
	unit: Unit;
	stage: Stage;
	/** Hours from pitch; blank is "part of the stage", not timed inside it. */
	offset: string;
	notes: string;
}

export function itemForm(item: RecipeItem | null, unit: Unit): ItemForm {
	return {
		ingredient_id: item?.ingredient_id ?? null,
		qty: show(item?.entry.value ?? null),
		unit: item?.entry.unit ?? unit,
		stage: item?.stage ?? "prep",
		offset: show(item?.offset_hours ?? null, undefined, 2),
		notes: item?.notes ?? "",
	};
}

export function toItemInput(form: ItemForm): Result<RecipeItemInput> {
	if (form.ingredient_id === null) return { ok: false, error: "Pick an ingredient" };
	if (parseNumber(form.qty) === null) return { ok: false, error: "A line needs a quantity" };
	const r = reader();
	const qty = r.num("quantity", form.qty) ?? 0;
	return r.done({
		ingredient_id: form.ingredient_id,
		entry: { value: qty, unit: form.unit },
		stage: form.stage,
		offset_hours: r.num("hours from pitch", form.offset),
		notes: form.notes.trim() === "" ? null : form.notes.trim(),
	});
}

/** Lines grouped by stage, in process order, empty stages left out. Within a stage the
 * backend's order holds: untimed first, then by hours from pitch. */
export function byStage<T extends { stage: Stage }>(items: T[]): { stage: Stage; items: T[] }[] {
	return STAGES.map((stage) => ({ stage, items: items.filter((i) => i.stage === stage) })).filter(
		(g) => g.items.length > 0,
	);
}

export function formatOffset(hours: number | null): string {
	if (hours === null) return "";
	const h = Number(hours.toFixed(2));
	return h < 0 ? `${-h} h before pitch` : `+${h} h`;
}

export function formatLine(item: RecipeItem): string {
	const at = formatOffset(item.offset_hours);
	return at ? `${formatQuantity(item.entry)} · ${at}` : formatQuantity(item.entry);
}

// -------------------------------------------------------------------- diff

const TARGET_LABELS: Record<TargetChange["field"], string> = {
	target_volume_l: "Volume",
	target_og: "OG",
	target_fg: "FG",
	target_abv: "ABV",
};

/** `{label, from, to}` strings for one changed target. Gravity in `unit`, ABV as percent. */
export function formatTarget(t: TargetChange, unit: string): { label: string; from: string; to: string } {
	const f = (v: number | null): string => {
		if (v === null) return "—";
		switch (t.field) {
			case "target_volume_l":
				return `${Number(v.toFixed(2))} L`;
			case "target_abv":
				return `${Number((v * 100).toFixed(2))} %`;
			default:
				return unit === "SG" ? v.toFixed(4) : `${v.toFixed(1)} ${unit}`;
		}
	};
	return { label: TARGET_LABELS[t.field], from: f(t.from), to: f(t.to) };
}

/** What moved in a changed line, said once: "2 g → 3 g", "+24 h → +36 h". */
export function describeChange(c: ItemChange): string {
	if (c.change === "added" && c.to) return formatLine(c.to);
	if (c.change === "removed" && c.from) return formatLine(c.from);
	if (!c.from || !c.to) return "";
	const parts: string[] = [];
	const qa = formatQuantity(c.from.entry);
	const qb = formatQuantity(c.to.entry);
	if (qa !== qb) parts.push(`${qa} → ${qb}`);
	if (c.from.offset_hours !== c.to.offset_hours) {
		parts.push(`${formatOffset(c.from.offset_hours) || "untimed"} → ${formatOffset(c.to.offset_hours) || "untimed"}`);
	}
	if ((c.from.notes ?? "") !== (c.to.notes ?? "")) parts.push("notes changed");
	return parts.join(", ");
}

/** The ingredients a line may use: active ones, plus the one it already has. */
export function pickable<T extends { id: number; archived_at: number | null }>(
	ingredients: T[],
	current: number | null,
): T[] {
	return ingredients.filter((i) => i.archived_at === null || i.id === current);
}

