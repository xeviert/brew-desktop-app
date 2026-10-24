import { describe, expect, it } from "vitest";
import type { Actual, LotChoice, PlanLine, RecipeItem } from "./api";
import { actualForm, byLineStage, deltaText, lotLabel, lotsFor, toActualInput, unitsFor } from "./actuals";

const lot = (over: Partial<LotChoice>): LotChoice => ({
	id: 1,
	ingredient_id: 10,
	supplier: "Metro",
	lot_code: null,
	qty: { mass: 5000 },
	entry: { value: 5, unit: "kg" },
	cost: { minor: 1250, currency: "EUR" },
	purchased_at: null,
	expires_at: null,
	notes: null,
	archived_at: null,
	ingredient: "Sucrose",
	category: "sugar",
	default_unit: "kg",
	...over,
});

const lots = [lot({}), lot({ id: 2, ingredient_id: 11, ingredient: "Fermaid O", default_unit: "g", lot_code: "FO-1" })];

const planned: RecipeItem = {
	id: 5,
	recipe_version_id: 1,
	ingredient_id: 11,
	qty: { mass: 10 },
	entry: { value: 10, unit: "g" },
	stage: "fermentation",
	offset_hours: 24,
	notes: null,
};

const actual: Actual = {
	id: 9,
	batch_id: 1,
	ingredient_lot_id: 1,
	ingredient_id: 10,
	qty: { mass: 4200 },
	entry: { value: 4.2, unit: "kg" },
	stage: "prep",
	added_at: null,
	notes: "two bags",
	ingredient: "Sucrose",
	lot_label: "Metro",
	lot_archived: false,
	line_cost: { minor: 1050, currency: "EUR" },
};

describe("the form", () => {
	it("prefills from a planned line with that ingredient's lot", () => {
		expect(actualForm({ planned }, lots)).toEqual({
			lot: 2,
			qty: "10",
			unit: "g",
			stage: "fermentation",
			at: "",
			notes: "",
		});
	});

	it("dates a timed planned line at pitch plus its offset, never in the future", () => {
		const pitch = new Date(2026, 7, 25, 21, 0).getTime() / 1000;
		expect(actualForm({ planned }, lots, pitch, pitch + 100 * 3_600).at).toBe("2026-08-26T21:00");
		expect(actualForm({ planned }, lots, pitch, pitch + 3_600).at).toBe("2026-08-25T22:00");
		expect(actualForm({ planned: { ...planned, offset_hours: null } }, lots, pitch).at).toBe("");
		expect(actualForm({ planned }, lots, null).at).toBe("");
	});

	it("round-trips an existing addition, and blank time stays untimed", () => {
		const f = actualForm({ actual }, lots);
		expect(toActualInput(f)).toEqual({
			ok: true,
			value: { ingredient_lot_id: 1, entry: { value: 4.2, unit: "kg" }, stage: "prep", added_at: null, notes: "two bags" },
		});
	});

	it("reads a decimal comma and a local time", () => {
		const r = toActualInput({ ...actualForm(null, lots), qty: "4,5", at: "2026-10-24T18:05" });
		expect(r).toMatchObject({
			ok: true,
			value: { entry: { value: 4.5, unit: "kg" }, added_at: new Date(2026, 9, 24, 18, 5).getTime() / 1000 },
		});
	});

	it("refuses no lot, no quantity and an unreadable one", () => {
		expect(toActualInput({ ...actualForm(null, []), qty: "1" })).toEqual({ ok: false, error: "Pick a lot" });
		expect(toActualInput(actualForm(null, lots))).toEqual({ ok: false, error: "An addition needs a quantity" });
		expect(toActualInput({ ...actualForm(null, lots), qty: "lots" })).toEqual({
			ok: false,
			error: "Not readable: quantity",
		});
	});

	it("offers one ingredient's lots for a planned line, and that ingredient's units", () => {
		expect(lotsFor(lots, 11).map((l) => l.id)).toEqual([2]);
		expect(lotsFor(lots, null)).toHaveLength(2);
		expect(unitsFor(lots, 2, "kg")).toContain("g");
		expect(unitsFor(lots, 2, "kg")).not.toContain("L");
		expect(lotLabel(lots[1])).toBe("Fermaid O — FO-1");
		expect(lotLabel(lots[0])).toBe("Sucrose — Metro");
	});
});

describe("the table", () => {
	const both = (delta_entry: number | null): PlanLine => ({
		kind: "both",
		planned: { ...planned, ingredient_id: 10, entry: { value: 4, unit: "kg" }, stage: "prep" },
		actual,
		ingredient: "Sucrose",
		delta: delta_entry === null ? null : delta_entry * 1000,
		delta_entry,
	});

	it("says a difference in the planned unit", () => {
		expect(deltaText(both(0.2))).toBe("+0.2 kg");
		expect(deltaText(both(-0.5))).toBe("−0.5 kg");
		expect(deltaText(both(0))).toBe("on plan");
		expect(deltaText(both(null))).toBeNull();
		expect(deltaText({ kind: "missing", planned, ingredient: "Fermaid O" })).toBeNull();
	});

	it("groups lines by the plan's stage, an unplanned one by its own", () => {
		const groups = byLineStage([
			{ kind: "missing", planned, ingredient: "Fermaid O" },
			both(0.2),
			{ kind: "unplanned", actual: { ...actual, stage: "packaging" }, ingredient: "Sucrose" },
		]);
		expect(groups.map((g) => [g.stage, g.items.map((l) => l.kind)])).toEqual([
			["prep", ["both"]],
			["fermentation", ["missing"]],
			["packaging", ["unplanned"]],
		]);
	});
});
