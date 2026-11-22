import { describe, expect, it } from "vitest";
import type { Ingredient, Lot, YeastStrain } from "./api";
import {
	formatUnitCost,
	ingredientForm,
	lotForm,
	moneyText,
	parseNumber,
	strainForm,
	toIngredientInput,
	toLotInput,
	toStrainInput,
	unitsLike,
} from "./reference";

const strain: YeastStrain = {
	id: 1,
	name: "EC-1118",
	lab: "Lallemand",
	form: "dry",
	temp_min_c: 10,
	temp_max_c: 30,
	attenuation_min: 0.78,
	attenuation_max: 1,
	alcohol_tolerance: 0.18,
	notes: null,
	archived_at: null,
};

const lot = (over: Partial<Lot>): Lot => ({
	id: 1,
	ingredient_id: 2,
	supplier: "Metro",
	lot_code: null,
	qty: { mass: 5000 },
	entry: { value: 5, unit: "kg" },
	cost: { minor: 1250, currency: "EUR" },
	purchased_at: null,
	expires_at: null,
	notes: null,
	archived_at: null,
	cost_per_canonical_unit: 0.25,
	batch_count: 0,
	stock: {
		lot_id: 1,
		bought: { value: 5, unit: "kg" },
		used: { value: 0, unit: "kg" },
		adjusted: { value: 0, unit: "kg" },
		remaining: { value: 5, unit: "kg" },
		value: { minor: 1250, currency: "EUR" },
	},
	...over,
});

describe("parseNumber", () => {
	it("reads a decimal comma and leaves blank absent", () => {
		expect(parseNumber("12,5")).toBe(12.5);
		expect(parseNumber(" 3 ")).toBe(3);
		expect(parseNumber("")).toBeNull();
		expect(parseNumber("1e3")).toBeNaN();
		expect(parseNumber("12abc")).toBeNaN();
	});
});

describe("strains", () => {
	it("shows fractions as percent and sends them back as fractions", () => {
		const form = strainForm(strain, "C");
		expect(form.attenuation_min).toBe("78");
		expect(form.alcohol_tolerance).toBe("18");
		const sent = toStrainInput(form, "C");
		expect(sent).toEqual({
			ok: true,
			value: {
				name: "EC-1118",
				lab: "Lallemand",
				form: "dry",
				temp_min_c: 10,
				temp_max_c: 30,
				attenuation_min: 0.78,
				attenuation_max: 1,
				alcohol_tolerance: 0.18,
				notes: null,
			},
		});
	});
	it("types temperatures in the display unit and stores °C", () => {
		const form = strainForm(strain, "F");
		expect(form.temp_min).toBe("50");
		expect(form.temp_max).toBe("86");
		const sent = toStrainInput({ ...form, temp_min: "59" }, "F");
		expect(sent.ok && sent.value.temp_min_c).toBeCloseTo(15);
	});
	it("names every unreadable field, and blanks are absent", () => {
		const form = { ...strainForm(null, "C"), name: "US-05", temp_min: "cold", attenuation_max: "8o" };
		expect(toStrainInput(form, "C")).toEqual({
			ok: false,
			error: "Not readable: temp min, attenuation max",
		});
		const blank = toStrainInput({ ...strainForm(null, "C"), name: " US-05 ", lab: "  " }, "C");
		expect(blank.ok && blank.value).toMatchObject({ name: "US-05", lab: null, attenuation_min: null });
	});
});

describe("ingredients", () => {
	const ingredient: Ingredient = {
		id: 3,
		name: "DAP",
		category: "nutrient",
		default_unit: "g",
		yeast_strain_id: null,
		attrs: '{"mg_yan_per_g":212}',
		notes: null,
		archived_at: null,
		yeast_strain: null,
		mg_yan_per_g: 212,
		lot_count: 1,
	};

	it("round-trips a nutrient's YAN", () => {
		const sent = toIngredientInput(ingredientForm(ingredient));
		expect(sent.ok && sent.value.mg_yan_per_g).toBe(212);
	});
	it("drops the fields of a category the form was switched away from", () => {
		const yeast = { ...ingredientForm(ingredient), category: "yeast" as const, yeast_strain_id: 1 };
		const sent = toIngredientInput(yeast);
		expect(sent.ok && sent.value).toMatchObject({ yeast_strain_id: 1, mg_yan_per_g: null });

		const sugar = toIngredientInput({ ...yeast, category: "sugar" });
		expect(sugar.ok && sugar.value.yeast_strain_id).toBeNull();
	});
	it("refuses an unreadable YAN rather than clearing it", () => {
		const form = { ...ingredientForm(ingredient), mg_yan_per_g: "lots" };
		expect(toIngredientInput(form)).toEqual({ ok: false, error: "Not readable: mg YAN per g" });
	});
	it("offers only units of the ingredient's dimension", () => {
		expect(unitsLike("kg")).toEqual(["mg", "g", "kg", "oz", "lb"]);
		expect(unitsLike("L")).toEqual(["mL", "L", "hL", "gal"]);
		expect(unitsLike("ea")).toEqual(["ea"]);
	});
});

describe("lots", () => {
	it("prefills the ingredient's unit and the default currency", () => {
		const form = lotForm(null, "kg", "EUR");
		expect(form).toMatchObject({ unit: "kg", currency: "EUR", cost: "", qty: "" });
	});
	it("sends the cost as typed, with its currency only when there is one", () => {
		const form = { ...lotForm(null, "kg", "eur"), qty: "2,5", cost: "12,50", purchased: "2026-10-01" };
		const sent = toLotInput(7, form);
		expect(sent).toEqual({
			ok: true,
			value: {
				ingredient_id: 7,
				supplier: null,
				lot_code: null,
				entry: { value: 2.5, unit: "kg" },
				cost: "12,50",
				currency: "EUR",
				purchased_at: new Date(2026, 9, 1).getTime() / 1000,
				expires_at: null,
				notes: null,
			},
		});
		const free = toLotInput(7, { ...form, cost: " " });
		expect(free.ok && free.value).toMatchObject({ cost: null, currency: null });
	});
	it("needs a readable quantity", () => {
		const form = lotForm(null, "kg", "EUR");
		expect(toLotInput(7, form)).toEqual({ ok: false, error: "A lot needs a quantity" });
		expect(toLotInput(7, { ...form, qty: "five" })).toEqual({
			ok: false,
			error: "Not readable: quantity",
		});
	});
	it("edits a stored cost in the currency's own digits", () => {
		expect(moneyText({ minor: 1250, currency: "EUR" })).toBe("12.50");
		expect(moneyText({ minor: 1250, currency: "JPY" })).toBe("1250");
		expect(lotForm(lot({}), "kg", "USD")).toMatchObject({ cost: "12.50", currency: "EUR", qty: "5" });
	});
	it("prices a bulk lot per kg, not per gram", () => {
		expect(formatUnitCost(lot({}))).toMatch(/2[.,]50/);
		expect(formatUnitCost(lot({}))).toMatch(/\/kg$/);
		expect(formatUnitCost(lot({ cost: null, cost_per_canonical_unit: null }))).toBeNull();
		expect(formatUnitCost(lot({ qty: { count: 10 }, cost_per_canonical_unit: 50 }))).toMatch(/\/unit$/);
	});
});
