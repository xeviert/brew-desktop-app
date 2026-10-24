/**
 * Phase 3 UI pass, U1. The ingredient, strain and lot forms, pure;
 * `IngredientsView.svelte` owns the calls.
 *
 * Forms hold what was typed — strings, temperatures in the display unit, attenuation in
 * percent, dates as local days — and these functions turn that into the canonical
 * payload. Range rules (fractions, min ≤ max, a positive quantity, the currency) are the
 * backend's, refused with a code; here, only "this is not a number" is caught, because
 * quietly sending `null` for an unreadable field would clear it.
 */
import type {
	Category,
	Ingredient,
	IngredientInput,
	Lot,
	LotInput,
	Quantity,
	Unit,
	YeastForm,
	YeastStrain,
	YeastStrainInput,
} from "$lib/api";
import { dayStart, toDate } from "$lib/query";
import {
	formatMoney,
	fromDisplayTemp,
	toDisplayTemp,
	type Currency,
	type Money,
	type TempDisplay,
} from "$lib/units";

export type Result<T> = { ok: true; value: T } | { ok: false; error: string };

export const CATEGORIES: { value: Category; label: string }[] = [
	{ value: "sugar", label: "Sugar" },
	{ value: "fermentable", label: "Fermentable" },
	{ value: "yeast", label: "Yeast" },
	{ value: "nutrient", label: "Nutrient" },
	{ value: "acid", label: "Acid" },
	{ value: "salt", label: "Salt" },
	{ value: "flavoring", label: "Flavoring" },
	{ value: "fining", label: "Fining" },
	{ value: "gas", label: "Gas" },
	{ value: "water", label: "Water" },
];

export const YEAST_FORMS: YeastForm[] = ["dry", "liquid", "slurry"];

/** Mirrors `brewcore::units::Unit::dimension`. */
const DIMENSIONS: Record<Unit, "mass" | "volume" | "count"> = {
	mg: "mass",
	g: "mass",
	kg: "mass",
	oz: "mass",
	lb: "mass",
	mL: "volume",
	L: "volume",
	hL: "volume",
	gal: "volume",
	ea: "count",
};

export const UNITS = Object.keys(DIMENSIONS) as Unit[];

/** The units a lot of this ingredient may be typed in. The backend still refuses the rest. */
export function unitsLike(unit: Unit): Unit[] {
	return UNITS.filter((u) => DIMENSIONS[u] === DIMENSIONS[unit]);
}

export function sameDimension(a: Unit, b: Unit): boolean {
	return DIMENSIONS[a] === DIMENSIONS[b];
}

/** `""` is absent; a decimal comma is a decimal point. `NaN` is unreadable. */
export function parseNumber(s: string): number | null {
	const t = s.trim().replace(",", ".");
	if (t === "") return null;
	return /^-?(\d+\.?\d*|\.\d+)$/.test(t) ? Number(t) : NaN;
}

/** Four decimals, so 0.78 × 100 shows as 78 and not 78.00000000000001. */
const show = (v: number | null, f: (v: number) => number = (v) => v): string =>
	v === null ? "" : String(Number(f(v).toFixed(4)));

const text = (s: string): string | null => (s.trim() === "" ? null : s.trim());

/** Collects unreadable fields by label, so the error names every one at once. */
export function reader() {
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
		date(label: string, s: string): number | null {
			if (s === "") return null;
			const v = dayStart(s);
			if (v === null) bad.push(label);
			return v;
		},
		done<T>(value: T): Result<T> {
			return bad.length ? { ok: false, error: `Not readable: ${bad.join(", ")}` } : { ok: true, value };
		},
	};
}

// ------------------------------------------------------------------ strains

export interface StrainForm {
	name: string;
	lab: string;
	form: YeastForm;
	/** In the display unit. */
	temp_min: string;
	temp_max: string;
	/** Percent: what the packet says. */
	attenuation_min: string;
	attenuation_max: string;
	alcohol_tolerance: string;
	notes: string;
}

export function strainForm(strain: YeastStrain | null, temp: TempDisplay): StrainForm {
	const t = (c: number) => toDisplayTemp(c, temp);
	const pct = (f: number) => f * 100;
	return {
		name: strain?.name ?? "",
		lab: strain?.lab ?? "",
		form: strain?.form ?? "dry",
		temp_min: show(strain?.temp_min_c ?? null, t),
		temp_max: show(strain?.temp_max_c ?? null, t),
		attenuation_min: show(strain?.attenuation_min ?? null, pct),
		attenuation_max: show(strain?.attenuation_max ?? null, pct),
		alcohol_tolerance: show(strain?.alcohol_tolerance ?? null, pct),
		notes: strain?.notes ?? "",
	};
}

export function toStrainInput(form: StrainForm, temp: TempDisplay): Result<YeastStrainInput> {
	const r = reader();
	const c = (v: number) => fromDisplayTemp(v, temp);
	const fraction = (v: number) => v / 100;
	return r.done({
		name: form.name.trim(),
		lab: text(form.lab),
		form: form.form,
		temp_min_c: r.num("temp min", form.temp_min, c),
		temp_max_c: r.num("temp max", form.temp_max, c),
		attenuation_min: r.num("attenuation min", form.attenuation_min, fraction),
		attenuation_max: r.num("attenuation max", form.attenuation_max, fraction),
		alcohol_tolerance: r.num("alcohol tolerance", form.alcohol_tolerance, fraction),
		notes: text(form.notes),
	});
}

export function strainLabel(strain: YeastStrain): string {
	return strain.lab ? `${strain.name} (${strain.lab})` : strain.name;
}

// -------------------------------------------------------------- ingredients

export interface IngredientForm {
	name: string;
	category: Category;
	default_unit: Unit;
	yeast_strain_id: number | null;
	mg_yan_per_g: string;
	notes: string;
}

export function ingredientForm(ingredient: Ingredient | null): IngredientForm {
	return {
		name: ingredient?.name ?? "",
		category: ingredient?.category ?? "sugar",
		default_unit: ingredient?.default_unit ?? "kg",
		yeast_strain_id: ingredient?.yeast_strain_id ?? null,
		mg_yan_per_g: show(ingredient?.mg_yan_per_g ?? null),
		notes: ingredient?.notes ?? "",
	};
}

/**
 * The strain goes only with a yeast and the YAN only with a nutrient, so a form switched
 * from one category to another does not carry a hidden field into the save.
 */
export function toIngredientInput(form: IngredientForm): Result<IngredientInput> {
	const r = reader();
	const nutrient = form.category === "nutrient";
	return r.done({
		name: form.name.trim(),
		category: form.category,
		default_unit: form.default_unit,
		yeast_strain_id: form.category === "yeast" ? form.yeast_strain_id : null,
		mg_yan_per_g: nutrient ? r.num("mg YAN per g", form.mg_yan_per_g) : null,
		notes: text(form.notes),
	});
}

// --------------------------------------------------------------------- lots

export interface LotForm {
	supplier: string;
	lot_code: string;
	qty: string;
	unit: Unit;
	/** As typed: `12,50`. */
	cost: string;
	currency: Currency;
	/** `yyyy-mm-dd`, as `<input type="date">`. */
	purchased: string;
	expires: string;
	notes: string;
}

/**
 * The digits after the point come from `Intl`, as `formatMoney` reads them, so JPY's
 * 1250 edits as `1250` and EUR's as `12.50`.
 */
export function moneyText(money: Money): string {
	let exponent = 2;
	try {
		exponent =
			new Intl.NumberFormat("en", { style: "currency", currency: money.currency }).resolvedOptions()
				.maximumFractionDigits ?? 2;
	} catch {
		// A code `Intl` doesn't know: the backend's default exponent.
	}
	return (money.minor / 10 ** exponent).toFixed(exponent);
}

export function lotForm(lot: Lot | null, unit: Unit, currency: Currency): LotForm {
	return {
		supplier: lot?.supplier ?? "",
		lot_code: lot?.lot_code ?? "",
		qty: show(lot?.entry.value ?? null),
		unit: lot?.entry.unit ?? unit,
		cost: lot?.cost ? moneyText(lot.cost) : "",
		currency: lot?.cost?.currency ?? currency,
		purchased: lot?.purchased_at != null ? toDate(lot.purchased_at) : "",
		expires: lot?.expires_at != null ? toDate(lot.expires_at) : "",
		notes: lot?.notes ?? "",
	};
}

/** A missing quantity is unreadable, not zero: a lot has to be of something. */
export function toLotInput(ingredientId: number, form: LotForm): Result<LotInput> {
	if (parseNumber(form.qty) === null) return { ok: false, error: "A lot needs a quantity" };
	const r = reader();
	// `null` here only when unreadable, which `done` then refuses.
	const qty = r.num("quantity", form.qty) ?? 0;
	const cost = text(form.cost);
	return r.done({
		ingredient_id: ingredientId,
		supplier: text(form.supplier),
		lot_code: text(form.lot_code),
		entry: { value: qty, unit: form.unit },
		cost,
		currency: cost === null ? null : form.currency.trim().toUpperCase(),
		purchased_at: r.date("purchased", form.purchased),
		expires_at: r.date("expires", form.expires),
		notes: text(form.notes),
	});
}

export function formatQuantity(q: Quantity): string {
	return `${Number(q.value.toFixed(4))} ${q.unit}`;
}

/**
 * Per kg, per L or per unit: a cost per gram reads €0.00. `cost_per_canonical_unit` is
 * minor units per gram / litre / unit, a float, and is rounded only here.
 */
export function formatUnitCost(lot: Lot): string | null {
	if (lot.cost === null || lot.cost_per_canonical_unit === null) return null;
	const [scale, per] =
		"mass" in lot.qty ? [1000, "kg"] : "volume" in lot.qty ? [1, "L"] : [1, "unit"];
	const minor = Math.round(lot.cost_per_canonical_unit * scale);
	return `${formatMoney({ minor, currency: lot.cost.currency })}/${per}`;
}
