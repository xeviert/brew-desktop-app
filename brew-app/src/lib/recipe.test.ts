import { describe, expect, it } from "vitest";
import type { ItemChange, RecipeItem, RecipeVersion } from "./api";
import {
	byStage,
	describeChange,
	formatLine,
	formatOffset,
	formatTarget,
	itemForm,
	pickable,
	targetsForm,
	toItemInput,
	toVersionInput,
} from "./recipe";

const version: RecipeVersion = {
	id: 1,
	recipe_id: 1,
	version_no: 1,
	locked_at: null,
	batch_count: 0,
	target_volume_l: 20,
	target_og: 1.038,
	target_fg: 0.998,
	target_abv: 0.0525,
	abv_alternate: 0.054,
	notes: null,
	gravity_unit: "SG",
	items: [],
};

const line = (over: Partial<RecipeItem>): RecipeItem => ({
	id: 1,
	recipe_version_id: 1,
	ingredient_id: 7,
	qty: { mass: 10 },
	entry: { value: 10, unit: "g" },
	stage: "fermentation",
	offset_hours: 24,
	notes: null,
	...over,
});

describe("targets", () => {
	it("shows ABV in percent and sends it back as a fraction", () => {
		const form = targetsForm(version);
		expect(form).toMatchObject({ volume: "20", og: "1.038", fg: "0.998", abv: "5.25" });
		expect(toVersionInput(form)).toEqual({
			ok: true,
			value: { target_volume_l: 20, target_og: 1.038, target_fg: 0.998, target_abv: 0.0525, notes: null },
		});
	});
	it("passes gravity through in the display unit — the backend converts", () => {
		const sent = toVersionInput({ ...targetsForm(version), og: "9,5", fg: "" });
		expect(sent.ok && sent.value).toMatchObject({ target_og: 9.5, target_fg: null });
	});
	it("names every unreadable field", () => {
		const form = { ...targetsForm(version), og: "high", abv: "5%" };
		expect(toVersionInput(form)).toEqual({ ok: false, error: "Not readable: OG, ABV" });
	});
});

describe("items", () => {
	it("round-trips a timed line", () => {
		const form = itemForm(line({}), "kg");
		expect(form).toMatchObject({ ingredient_id: 7, qty: "10", unit: "g", offset: "24" });
		expect(toItemInput(form)).toEqual({
			ok: true,
			value: { ingredient_id: 7, entry: { value: 10, unit: "g" }, stage: "fermentation", offset_hours: 24, notes: null },
		});
	});
	it("starts a new line in the ingredient's unit, untimed", () => {
		expect(itemForm(null, "kg")).toMatchObject({ ingredient_id: null, unit: "kg", stage: "prep", offset: "" });
	});
	it("needs an ingredient and a readable quantity", () => {
		expect(toItemInput(itemForm(null, "kg"))).toEqual({ ok: false, error: "Pick an ingredient" });
		const form = { ...itemForm(null, "kg"), ingredient_id: 3 };
		expect(toItemInput(form)).toEqual({ ok: false, error: "A line needs a quantity" });
		expect(toItemInput({ ...form, qty: "4", offset: "soon" })).toEqual({
			ok: false,
			error: "Not readable: hours from pitch",
		});
	});
	it("accepts a negative offset: before pitch", () => {
		const sent = toItemInput({ ...itemForm(null, "g"), ingredient_id: 3, qty: "2", offset: "-2" });
		expect(sent.ok && sent.value.offset_hours).toBe(-2);
		expect(formatOffset(-2)).toBe("2 h before pitch");
		expect(formatOffset(24)).toBe("+24 h");
		expect(formatLine(line({ offset_hours: null }))).toBe("10 g");
	});
	it("groups by stage in process order and drops empty stages", () => {
		const groups = byStage([line({ stage: "fermentation" }), line({ stage: "prep" }), line({ stage: "prep" })]);
		expect(groups.map((g) => [g.stage, g.items.length])).toEqual([
			["prep", 2],
			["fermentation", 1],
		]);
	});
	it("offers active ingredients plus the line's own archived one", () => {
		const all = [
			{ id: 1, archived_at: null },
			{ id: 2, archived_at: 5 },
			{ id: 3, archived_at: 5 },
		];
		expect(pickable(all, 2).map((i) => i.id)).toEqual([1, 2]);
		expect(pickable(all, null).map((i) => i.id)).toEqual([1]);
	});
});

describe("diff", () => {
	it("says a moved addition once, as what moved", () => {
		const moved: ItemChange = {
			change: "changed",
			ingredient: "DAP",
			stage: "fermentation",
			from: line({}),
			to: line({ offset_hours: 36 }),
		};
		expect(describeChange(moved)).toBe("+24 h → +36 h");
		const both: ItemChange = { ...moved, to: line({ entry: { value: 15, unit: "g" }, offset_hours: null }) };
		expect(describeChange(both)).toBe("10 g → 15 g, +24 h → untimed");
		expect(describeChange({ ...moved, change: "added", from: null })).toBe("10 g · +36 h");
		expect(describeChange({ ...moved, change: "removed", to: null })).toBe("10 g · +24 h");
	});
	it("formats targets in their own units", () => {
		expect(formatTarget({ field: "target_og", from: 1.038, to: 1.04 }, "SG")).toEqual({
			label: "OG",
			from: "1.0380",
			to: "1.0400",
		});
		expect(formatTarget({ field: "target_fg", from: null, to: 0.5 }, "°P")).toEqual({
			label: "FG",
			from: "—",
			to: "0.5 °P",
		});
		expect(formatTarget({ field: "target_abv", from: 0.05, to: 0.055 }, "SG").to).toBe("5.5 %");
	});
});
