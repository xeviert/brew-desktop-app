/**
 * Phase 7c. What is left in a lot, pure; `LotsPanel.svelte` and `LotLedger.svelte` own the
 * calls. Remaining is the backend's sum (`db::inventory`), in the lot's own unit; this file
 * words it and reads the adjust and stocktake forms.
 */
import type { AdjustmentInput, Quantity, Unit } from "$lib/api";
import { formatQuantity, parseNumber, type Result } from "$lib/reference";

/** A hair over zero is float noise from summing grams, not stock. */
const EPSILON = 1e-6;

/** "0.75 kg left", "empty", or "over by 0.2 kg" when the books went below zero. */
export function remainingText(remaining: Quantity): { text: string; over: boolean; empty: boolean } {
	const v = remaining.value;
	if (v < -EPSILON) return { text: `over by ${formatQuantity({ ...remaining, value: -v })}`, over: true, empty: true };
	if (v <= EPSILON) return { text: "empty", over: false, empty: true };
	return { text: `${formatQuantity(remaining)} left`, over: false, empty: false };
}

export interface AdjustForm {
	/** "remove" takes the quantity out; "count" sets what is left to it. */
	mode: "remove" | "count";
	qty: string;
	unit: Unit;
	reason: string;
}

export const adjustForm = (unit: Unit): AdjustForm => ({ mode: "remove", qty: "", unit, reason: "" });

export type AdjustInput = { mode: "remove"; adjustment: AdjustmentInput } | { mode: "count"; counted: Quantity; reason: string | null };

export function toAdjustInput(form: AdjustForm): Result<AdjustInput> {
	const qty = parseNumber(form.qty);
	if (qty === null) return { ok: false, error: "Give a quantity" };
	if (Number.isNaN(qty)) return { ok: false, error: "Not readable: quantity" };
	const reason = form.reason.trim();
	if (form.mode === "count") {
		if (qty < 0) return { ok: false, error: "A count can't be below zero" };
		return { ok: true, value: { mode: "count", counted: { value: qty, unit: form.unit }, reason: reason || null } };
	}
	if (reason === "") return { ok: false, error: "Say why: spilt, gave away, expired" };
	return { ok: true, value: { mode: "remove", adjustment: { entry: { value: qty, unit: form.unit }, reason } } };
}

/** An adjustment as the ledger lists it: "−200 g" taken, "+50 g" found. */
export function adjustmentText(entry: Quantity): string {
	return entry.value >= 0
		? `−${formatQuantity(entry)}`
		: `+${formatQuantity({ ...entry, value: -entry.value })}`;
}
