/**
 * Phase 3 UI pass, U5. The additions form and the plan-vs-actual wording, pure;
 * `Actuals.svelte` owns the calls.
 *
 * A quantity is sent as typed and converted by the backend through the lot's
 * ingredient; the picker only offers units of that dimension, and the backend's
 * `units.dimension_mismatch` stays the enforcement. Positive quantities, an archived lot
 * and a future time are the backend's to refuse.
 */
import type { Actual, ActualInput, LotChoice, PlanLine, RecipeItem, Stage, Unit } from "$lib/api";
import { fromLocalDateTime, localDateTime } from "$lib/batch";
import { byStage } from "$lib/recipe";
import { formatQuantity, parseNumber, unitsLike, type Result } from "$lib/reference";

export interface ActualForm {
	lot: number | null;
	qty: string;
	unit: Unit;
	stage: Stage;
	/** `datetime-local`; blank is untimed. */
	at: string;
	notes: string;
}

/** A blank form, or one prefilled from a planned line ("record this") or an existing
 * addition ("edit"). A planned line picks the first lot of its ingredient, and a timed
 * one its planned time — pitch plus the offset, no later than now. Pairing with the plan
 * goes by order within (ingredient, stage), and an untimed addition sorts first, so the
 * second of two nutrient doses recorded blank would pair with the first. */
export function actualForm(
	from: { planned: RecipeItem } | { actual: Actual } | null,
	lots: LotChoice[],
	pitchedAt: number | null = null,
	now: number = Date.now() / 1000,
): ActualForm {
	if (from && "actual" in from) {
		const a = from.actual;
		return {
			lot: a.ingredient_lot_id,
			qty: String(a.entry.value),
			unit: a.entry.unit,
			stage: a.stage,
			at: a.added_at === null ? "" : localDateTime(a.added_at),
			notes: a.notes ?? "",
		};
	}
	if (from) {
		const p = from.planned;
		const due = pitchedAt !== null && p.offset_hours !== null ? pitchedAt + p.offset_hours * 3_600 : null;
		return {
			lot: lots.find((l) => l.ingredient_id === p.ingredient_id)?.id ?? null,
			qty: String(p.entry.value),
			unit: p.entry.unit,
			stage: p.stage,
			at: due === null ? "" : localDateTime(Math.min(due, now)),
			notes: "",
		};
	}
	const first = lots[0];
	return { lot: first?.id ?? null, qty: "", unit: first?.default_unit ?? "g", stage: "prep", at: "", notes: "" };
}

/** The lots a form may pick: one ingredient's when the line is planned, else all. An
 * existing addition's own lot stays offered even if it has left the picker. */
export function lotsFor(lots: LotChoice[], ingredientId: number | null): LotChoice[] {
	return ingredientId === null ? lots : lots.filter((l) => l.ingredient_id === ingredientId);
}

export function hasLot(lots: LotChoice[], ingredientId: number): boolean {
	return lots.some((l) => l.ingredient_id === ingredientId);
}

/** "10 g · Boil 60 min": what the recipe asked for, for the form's heading. */
export function planSummary(p: RecipeItem): string {
	return p.notes ? `${formatQuantity(p.entry)} · ${p.notes}` : formatQuantity(p.entry);
}

/** Units the picked lot's ingredient may be typed in. */
export function unitsFor(lots: LotChoice[], lot: number | null, fallback: Unit): Unit[] {
	const picked = lots.find((l) => l.id === lot);
	return unitsLike(picked?.default_unit ?? fallback);
}

export function lotLabel(l: LotChoice): string {
	const tag = l.lot_code ?? l.supplier;
	return tag ? `${l.ingredient} — ${tag}` : `${l.ingredient} — lot ${l.id}`;
}

export function toActualInput(form: ActualForm): Result<ActualInput> {
	if (form.lot === null) return { ok: false, error: "Pick a lot" };
	const qty = parseNumber(form.qty);
	if (qty === null) return { ok: false, error: "An addition needs a quantity" };
	const bad: string[] = [];
	if (Number.isNaN(qty)) bad.push("quantity");
	let added_at: number | null = null;
	if (form.at !== "") {
		added_at = fromLocalDateTime(form.at);
		if (added_at === null) bad.push("time");
	}
	if (bad.length) return { ok: false, error: `Not readable: ${bad.join(", ")}` };
	return {
		ok: true,
		value: {
			ingredient_lot_id: form.lot,
			entry: { value: qty, unit: form.unit },
			stage: form.stage,
			added_at,
			notes: form.notes.trim() === "" ? null : form.notes.trim(),
		},
	};
}

// -------------------------------------------------------------- the table

/** A line's stage: the plan's where there is one. */
export function stageOf(line: PlanLine): Stage {
	return line.kind === "unplanned" ? line.actual.stage : line.planned.stage;
}

export function byLineStage(lines: PlanLine[]): { stage: Stage; items: PlanLine[] }[] {
	return byStage(lines.map((l) => ({ ...l, stage: stageOf(l) }))) as { stage: Stage; items: PlanLine[] }[];
}

/** "+0.2 kg", "−10 g", "on plan". `null` when the two cannot be compared. */
export function deltaText(line: PlanLine): string | null {
	if (line.kind !== "both" || line.delta_entry === null) return null;
	const d = Number(line.delta_entry.toFixed(4));
	if (d === 0) return "on plan";
	return `${d > 0 ? "+" : "−"}${formatQuantity({ value: Math.abs(d), unit: line.planned.entry.unit })}`;
}
