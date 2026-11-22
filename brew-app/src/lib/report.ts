/**
 * Phase 7e. The batch report, pure: `BatchReport.svelte` fetches what the batch screen
 * already shows, and this file turns it into one record — the screen's sheet, and the same
 * record as plain text for pasting into notes. Lists, not tables, in the text: a notes app
 * keeps lines, not columns.
 *
 * No new backend aggregate: every figure here is one some other screen already shows, so a
 * report can never disagree with the screen it was printed from.
 */
import type {
	Actual,
	Actuals,
	BatchSummary,
	Cleaning,
	Curve,
	Dashboard,
	Packaging,
	PlanLine,
	Series,
	Timeline,
	TimelineItem,
} from "$lib/api";
import { recipeLabel } from "$lib/batch";
import { cleaningLabel } from "$lib/cleaning";
import { lineText, lossText } from "$lib/packaging";
import { formatQuantity } from "$lib/reference";
import { formatMoney, formatTemp, formatTimestamp, formatVolume, type TempDisplay } from "$lib/units";

const HOUR = 3_600;
const DAY = 24 * HOUR;

export interface Fact {
	label: string;
	value: string;
}

export interface FermentationRow {
	/** "0 h", "12 h", "Day 1", … counted from the pitch. */
	at: string;
	gravity: string | null;
	temp: string | null;
	ph: string | null;
}

export interface Report {
	code: string;
	facts: Fact[];
	fermentation: FermentationRow[];
	gravityUnit: string;
	ingredients: { stage: string; text: string; cost: string | null }[];
	cost: string | null;
	events: { when: string; text: string }[];
	packaging: string[];
	packagingSummary: string | null;
	hygiene: string | null;
}

/**
 * When to read the curve: the pitch, 12 h, then each day — the rhythm a brewer checks at.
 * After four weeks, weekly: a long conditioning would otherwise be a page of flat lines.
 */
export function sampleTimes(pitchedAt: number, end: number): number[] {
	if (end < pitchedAt) return [];
	const out = [pitchedAt, pitchedAt + 12 * HOUR];
	for (let day = 1; pitchedAt + day * DAY <= end; day += day < 28 ? 1 : 7) out.push(pitchedAt + day * DAY);
	return out.filter((t) => t <= end);
}

export function sampleLabel(t: number, pitchedAt: number): string {
	const hours = Math.round((t - pitchedAt) / HOUR);
	return hours < 24 ? `${hours} h` : `Day ${Math.round(hours / 24)}`;
}

/**
 * The value nearest `t` within `window` seconds, across the series given in order of
 * preference: a sensor's smoothed line before its raw points, a sensor before a hand reading.
 */
export function valueAt(series: Series[], t: number, window: number): number | null {
	for (const s of series) {
		const values = s.smoothed.length > 0 ? s.smoothed : s.avg;
		let best: number | null = null;
		let distance = Infinity;
		s.ts.forEach((ts, i) => {
			const v = values[i];
			const d = Math.abs(ts - t);
			if (v !== null && d <= window && d < distance) {
				best = v;
				distance = d;
			}
		});
		if (best !== null) return best;
	}
	return null;
}

/** Sensors first; manual readings fill in where there is no sensor, or it was silent. */
function byPreference(curve: Curve, metric: string): Series[] {
	const of = curve.series.filter((s) => s.metric === metric);
	return [...of.filter((s) => s.source_kind !== "manual"), ...of.filter((s) => s.source_kind === "manual")];
}

export function fermentationRows(curve: Curve, pitchedAt: number, end: number, temp: TempDisplay): FermentationRow[] {
	const gravity = byPreference(curve, "sg");
	const vessel = byPreference(curve, "temp_c");
	const ph = byPreference(curve, "ph");
	const digits = curve.gravity_unit === "SG" ? 3 : 1;
	return sampleTimes(pitchedAt, end)
		.map((t) => {
			// A sensor reads every few minutes; a hand reading is a day's figure.
			const g = valueAt(gravity, t, 6 * HOUR);
			const c = valueAt(vessel, t, 3 * HOUR);
			const p = valueAt(ph, t, 12 * HOUR);
			return {
				at: sampleLabel(t, pitchedAt),
				gravity: g === null ? null : g.toFixed(digits),
				temp: c === null ? null : formatTemp(c, temp),
				ph: p === null ? null : `pH ${p.toFixed(2)}`,
			};
		})
		.filter((r) => r.gravity !== null || r.temp !== null || r.ph !== null);
}

/** The newest sanitise at or before the pitch, said against the pitch. */
export function hygieneAtPitch(entries: Cleaning[], pitchedAt: number | null, vessel: string): string | null {
	if (pitchedAt === null) return null;
	const before = entries.filter((e) => e.cleaned_at <= pitchedAt).sort((a, b) => b.cleaned_at - a.cleaned_at);
	const sanitised = before.find((e) => e.kind === "sanitize" || e.kind === "clean_and_sanitize");
	if (!sanitised) return `${vessel}: no sanitise logged before the pitch`;
	const hours = Math.round((pitchedAt - sanitised.cleaned_at) / HOUR);
	const ago = hours < 48 ? `${hours} h` : `${Math.round(hours / 24)} d`;
	const what = cleaningLabel(sanitised.kind).toLowerCase();
	return `${vessel}: ${what} ${ago} before the pitch${sanitised.chemical ? ` (${sanitised.chemical})` : ""}`;
}

function actualOf(line: PlanLine): Actual | null {
	return line.kind === "missing" ? null : line.actual;
}

export interface ReportInput {
	batch: BatchSummary;
	dashboard: Dashboard;
	timeline: Timeline;
	actuals: Actuals;
	packaging: Packaging;
	curve: Curve;
	cleaning: Cleaning[] | null;
	temp: TempDisplay;
	now: number;
}

export function buildReport(input: ReportInput): Report {
	const { batch, dashboard, timeline, actuals, packaging, curve, temp } = input;
	const unit = dashboard.gravity_unit;
	const g = (v: number | null) => (v === null ? null : unit === "SG" ? v.toFixed(3) : `${v.toFixed(1)} ${unit}`);

	const packagedAt = packaging.lines.length > 0 ? Math.min(...packaging.lines.map((l) => l.at)) : null;
	const facts: Fact[] = [
		{ label: "Recipe", value: recipeLabel(batch) },
		{ label: "Type", value: batch.beverage_type },
		{ label: "Status", value: batch.status + (batch.archived_at !== null ? " (archived)" : "") },
	];
	if (batch.vessel_name) facts.push({ label: "Vessel", value: batch.vessel_name });
	if (batch.volume_l !== null) facts.push({ label: "Volume", value: formatVolume(batch.volume_l) });
	if (batch.pitched_at !== null) facts.push({ label: "Pitched", value: formatTimestamp(batch.pitched_at) });
	if (packagedAt !== null) facts.push({ label: "Packaged", value: formatTimestamp(packagedAt) });
	const og = g(dashboard.display.og);
	if (og) facts.push({ label: "OG", value: og });
	const fg = g(dashboard.display.fg) ?? (dashboard.display.gravity !== null ? `${g(dashboard.display.gravity)} (latest)` : null);
	if (fg) facts.push({ label: dashboard.fg !== null ? "FG" : "Gravity", value: fg });
	if (dashboard.estimate.state === "estimated") {
		facts.push({ label: "ABV", value: `${(dashboard.estimate.abv_alternate * 100).toFixed(1)} %` });
		facts.push({ label: "Attenuation", value: `${(dashboard.estimate.apparent_attenuation * 100).toFixed(0)} %` });
	}

	const end = packagedAt ?? (curve.range ? curve.range.end : input.now);
	const fermentation =
		batch.pitched_at === null ? [] : fermentationRows(curve, batch.pitched_at, Math.min(end, input.now), temp);

	const ingredients = actuals.lines
		.map(actualOf)
		.filter((a): a is Actual => a !== null)
		.map((a) => ({
			stage: a.stage,
			text: `${a.ingredient}, ${formatQuantity(a.entry)} (${a.lot_label}${a.added_at !== null ? `, ${formatTimestamp(a.added_at)}` : ""})`,
			cost: a.line_cost ? formatMoney(a.line_cost) : null,
		}));

	let cost: string | null = null;
	if (actuals.cost_error) cost = actuals.cost_error.message;
	else if (actuals.cost?.total) {
		cost = formatMoney(actuals.cost.total);
		if (actuals.cost.per_litre) cost += `, ${formatMoney(actuals.cost.per_litre)}/L brewed`;
		if (packaging.cost?.per_packaged_l) cost += `, ${formatMoney(packaging.cost.per_packaged_l)}/L packaged`;
		if (actuals.cost.lines_without_cost > 0) cost += ` (${actuals.cost.lines_without_cost} unpriced)`;
	}

	const events = timeline.items
		.filter((i): i is Extract<TimelineItem, { type: "event" }> => i.type === "event")
		.map((e) => ({
			when: formatTimestamp(e.ts),
			text: `${e.kind.replaceAll("_", " ")}${e.note ? `: ${e.note}` : ""}`,
		}));

	let packagingSummary: string | null = null;
	if (packaging.lines.length > 0) {
		packagingSummary = `${formatVolume(packaging.packaged_l)} packaged`;
		const loss = lossText(packaging);
		if (loss) packagingSummary += `, ${loss}`;
	}

	return {
		code: batch.code,
		facts,
		fermentation,
		gravityUnit: unit,
		ingredients,
		cost,
		events,
		packaging: packaging.lines.map(
			(l) => `${formatTimestamp(l.at, { dateStyle: "medium" })}: ${lineText(l)}, ${formatVolume(l.volume_l)}${l.notes ? ` (${l.notes})` : ""}`,
		),
		packagingSummary,
		hygiene: input.cleaning && batch.vessel_name ? hygieneAtPitch(input.cleaning, batch.pitched_at, batch.vessel_name) : null,
	};
}

/** The report as lines to paste into notes: a heading per section, one fact per line. */
export function reportText(r: Report): string {
	const out: string[] = [`Batch ${r.code}`, ""];
	for (const f of r.facts) out.push(`${f.label}: ${f.value}`);
	if (r.hygiene) out.push(r.hygiene);

	const section = (title: string, lines: string[]) => {
		if (lines.length === 0) return;
		out.push("", `${title}:`, ...lines);
	};
	section(
		"Fermentation",
		r.fermentation.map((row) => `${row.at}: ${[row.gravity, row.temp, row.ph].filter(Boolean).join(", ")}`),
	);
	section(
		"Ingredients",
		r.ingredients.map((i) => `${i.stage}: ${i.text}${i.cost ? `, ${i.cost}` : ""}`),
	);
	if (r.cost) out.push(`Cost: ${r.cost}`);
	section("Events", r.events.map((e) => `${e.when}: ${e.text}`));
	section("Packaging", r.packaging);
	if (r.packagingSummary) out.push(r.packagingSummary);
	return out.join("\n");
}
