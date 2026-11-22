/**
 * U7. The calculators' forms and wording, pure; `CalcView.svelte` owns the calls.
 *
 * Every number is computed in Rust by `brewcore::calc`, and every preset comes from
 * `calc_defaults` — nothing here knows a demand factor or a pitch rate. Gravities are
 * typed in `ui.gravity_display`; percent fields are sent as fractions. A form with a
 * required field blank is `null`, not an error: the panel is still being filled in.
 */
import type { PitchInput, RecipeVersion, YanInput } from "$lib/api";
import { reader, type Result } from "$lib/reference";
import { formatMass } from "$lib/units";

const show = (v: number | null, digits = 4): string => (v === null ? "" : String(Number(v.toFixed(digits))));

const pct = (v: number) => v / 100;

/** Blank required fields mean "not yet", so no call is made. */
function ready<T>(fields: (number | null)[], r: ReturnType<typeof reader>, value: () => T): Result<T> | null {
	const result = r.done(null);
	if (!result.ok) return result;
	return fields.some((f) => f === null) ? null : { ok: true, value: value() };
}

/** What a recipe version can prefill: its target OG and volume, both may be unset. */
export function fromVersion(v: RecipeVersion): { gravity: string; volume: string } {
	return { gravity: show(v.target_og), volume: show(v.target_volume_l, 2) };
}

// ------------------------------------------------------------------ sugar

/** `target`: the sugar for an OG. `og`: the OG a weighed sugar gives. */
export type SugarMode = "target" | "og";

export interface SugarForm {
	mode: SugarMode;
	og: string;
	/** kg, as a sack is weighed. */
	sugar: string;
	/** Water added, not the finished volume: the sugar adds its own. */
	water: string;
}

export type SugarArgs = { mode: "target"; og: number; water: number } | { mode: "og"; sugarG: number; water: number };

export function toSugarArgs(form: SugarForm): Result<SugarArgs> | null {
	const r = reader();
	const water = r.num("water", form.water);
	if (form.mode === "target") {
		const og = r.num("target OG", form.og);
		return ready([og, water], r, () => ({ mode: "target", og: og!, water: water! }));
	}
	const sugarG = r.num("sugar", form.sugar, (kg) => kg * 1000);
	return ready([sugarG, water], r, () => ({ mode: "og", sugarG: sugarG!, water: water! }));
}

// ------------------------------------------------------------------ YAN

export interface YanForm {
	gravity: string;
	volume: string;
	/** mg N per gram of sugar, as `calc_defaults`' presets give it. */
	demand: string;
	mgYanPerG: string;
	/** How many equal doses. */
	doses: string;
}

/** `n` equal fractions. TOSNA's four quarters are the case `n = 4`. */
export function equalDoses(n: number): number[] {
	return Array.from({ length: n }, () => 1 / n);
}

export function toYanInput(form: YanForm): Result<YanInput> | null {
	const r = reader();
	const gravity = r.num("gravity", form.gravity);
	const volume = r.num("volume", form.volume);
	const demand = r.num("demand", form.demand);
	const yieldPerG = r.num("nutrient YAN", form.mgYanPerG);
	const doses = r.num("doses", form.doses);
	if (doses !== null && !(Number.isInteger(doses) && doses >= 1)) {
		return { ok: false, error: "Doses: a whole number, 1 or more" };
	}
	return ready([gravity, volume, demand, yieldPerG, doses], r, () => ({
		gravity: gravity!,
		volume_l: volume!,
		demand: demand!,
		mg_yan_per_g: yieldPerG!,
		doses: equalDoses(doses!),
	}));
}

// ------------------------------------------------------------------ pitch

export interface PitchForm {
	gravity: string;
	volume: string;
	/** Million cells per mL per °P. */
	rate: string;
	/** Dry yeast: grams are worked out from the two fields below. */
	dry: boolean;
	/** Billion cells per gram, as a packet states it. */
	cellsPerGram: string;
	/** Percent. Blank is unknown, and then no grams are shown. */
	viability: string;
}

export function toPitchInput(form: PitchForm): Result<PitchInput> | null {
	const r = reader();
	const gravity = r.num("gravity", form.gravity);
	const volume = r.num("volume", form.volume);
	const rate = r.num("rate", form.rate);
	const perGram = form.dry ? r.num("cells per gram", form.cellsPerGram, (b) => b * 1e9) : null;
	const viability = form.dry ? r.num("viability", form.viability, pct) : null;
	return ready([gravity, volume, rate], r, () => ({
		gravity: gravity!,
		volume_l: volume!,
		rate: rate!,
		cells_per_gram: perGram,
		viability,
	}));
}

/** The packet figure is typed in billions; `calc_defaults` gives cells. */
export const billions = (cells: number): string => show(cells / 1e9, 1);

// ------------------------------------------------------------------ wording

export function formatCells(cells: number): string {
	return `${(cells / 1e9).toFixed(0)} billion cells`;
}

/** Doses under 10 g to two places: a 5.77 g addition is weighed on a jeweller's scale. */
export function formatDose(grams: number): string {
	return grams < 10 ? `${grams.toFixed(2)} g` : formatMass(grams);
}

export function formatPpm(ppm: number): string {
	return `${ppm.toFixed(0)} mg/L`;
}
