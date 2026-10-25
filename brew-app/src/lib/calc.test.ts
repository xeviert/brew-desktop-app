import { describe, expect, it } from "vitest";
import type { RecipeVersion } from "./api";
import {
	billions,
	equalDoses,
	formatCells,
	formatDose,
	fromVersion,
	toPitchInput,
	toSugarArgs,
	toYanInput,
	type PitchForm,
	type YanForm,
} from "./calc";

const version: RecipeVersion = {
	id: 4,
	recipe_id: 1,
	version_no: 1,
	locked_at: null,
	batch_count: 0,
	target_volume_l: 20,
	target_og: 1.0751,
	target_fg: null,
	target_abv: null,
	abv_alternate: null,
	notes: null,
	gravity_unit: "SG",
	items: [],
};

const yan: YanForm = { gravity: "1.075", volume: "20", demand: "1.25", mgYanPerG: "212", doses: "4" };
const pitch: PitchForm = {
	gravity: "1,048",
	volume: "20",
	rate: "0.75",
	dry: true,
	cellsPerGram: "6",
	viability: "",
};

describe("prefill", () => {
	it("takes the version's target OG and volume, blank when unset", () => {
		expect(fromVersion(version)).toEqual({ gravity: "1.0751", volume: "20" });
		expect(fromVersion({ ...version, target_og: null, target_volume_l: null })).toEqual({ gravity: "", volume: "" });
	});
});

describe("sugar", () => {
	it("sends the OG for a target and kilograms as grams for a weighed sugar", () => {
		expect(toSugarArgs({ mode: "target", og: "1,075", sugar: "", water: "20" })).toEqual({
			ok: true,
			value: { mode: "target", og: 1.075, water: 20 },
		});
		expect(toSugarArgs({ mode: "og", og: "", sugar: "4,2", water: "20" })).toEqual({
			ok: true,
			value: { mode: "og", sugarG: 4200, water: 20 },
		});
	});
	it("waits for a blank field and names an unreadable one", () => {
		expect(toSugarArgs({ mode: "target", og: "1.075", sugar: "", water: "" })).toBeNull();
		expect(toSugarArgs({ mode: "target", og: "abc", sugar: "", water: "20" })).toEqual({
			ok: false,
			error: "Not readable: target OG",
		});
	});
});

describe("YAN", () => {
	it("splits into equal fractions that sum to one", () => {
		expect(equalDoses(4)).toEqual([0.25, 0.25, 0.25, 0.25]);
		expect(equalDoses(3).reduce((a, b) => a + b)).toBeCloseTo(1, 12);
	});
	it("shapes the input", () => {
		expect(toYanInput(yan)).toEqual({
			ok: true,
			value: { gravity: 1.075, volume_l: 20, demand: 1.25, mg_yan_per_g: 212, doses: [0.25, 0.25, 0.25, 0.25] },
		});
	});
	it("refuses a dose count that is not a whole number", () => {
		expect(toYanInput({ ...yan, doses: "2.5" })).toEqual({ ok: false, error: "Doses: a whole number, 1 or more" });
		expect(toYanInput({ ...yan, doses: "0" })).toMatchObject({ ok: false });
		expect(toYanInput({ ...yan, mgYanPerG: "" })).toBeNull();
	});
});

describe("pitch", () => {
	it("sends billions as cells and viability as a fraction", () => {
		expect(toPitchInput({ ...pitch, viability: "80" })).toEqual({
			ok: true,
			value: { gravity: 1.048, volume_l: 20, rate: 0.75, cells_per_gram: 6e9, viability: 0.8 },
		});
	});
	it("leaves viability unset when blank, so no grams come back", () => {
		const input = toPitchInput(pitch);
		expect(input?.ok && input.value.viability).toBeNull();
	});
	it("sends neither dry-yeast field for a liquid yeast", () => {
		const input = toPitchInput({ ...pitch, dry: false, viability: "80" });
		expect(input?.ok && [input.value.cells_per_gram, input.value.viability]).toEqual([null, null]);
	});
});

describe("wording", () => {
	it("says cells in billions and small doses to two places", () => {
		expect(formatCells(1.8e11)).toBe("180 billion cells");
		expect(billions(6e9)).toBe("6");
		expect(formatDose(5.7712)).toBe("5.77 g");
		expect(formatDose(23.1)).toBe("23.1 g");
		expect(formatDose(4200)).toBe("4.2 kg");
	});
});
