/**
 * Phase 5b. `CompareView` → one uPlot data set per panel. Pure, like `chart.ts`;
 * `CompareView.svelte` owns uPlot.
 *
 * Panels, not a second y-axis: attenuation, SG and temperature are different
 * measures, so each gets its own chart on one shared x (hours since pitch) with a
 * synced cursor.
 */
import type { CompareView, Series } from "$lib/api";
import { align, toX } from "$lib/chart";
import { toDisplayTemp, type TempDisplay } from "$lib/units";

export type Panel = "frac" | "grav" | "temp";

/** Categorical, fixed order, validated against the dark surface. Eight = `MAX_BATCHES`. */
export const PALETTE = [
	"#3987e5",
	"#d95926",
	"#199e70",
	"#c98500",
	"#d55181",
	"#008300",
	"#9085e9",
	"#e66767",
];

export interface CompareDrawn {
	batchId: number;
	code: string;
	color: string;
	/** Sensor gravity and temp are lines; manual readings are points. */
	role: "line" | "manual";
}

export interface ComparePanel {
	panel: Panel;
	/** `[x, ...columns]`, x shared by every panel so the cursors line up. */
	data: (number | null | undefined)[][];
	drawn: CompareDrawn[];
}

/**
 * Colour follows the batch: a batch keeps its slot while it stays picked, and a
 * newcomer takes the lowest free one, so removing a batch never repaints the rest.
 */
export function assignSlots(prev: Map<number, number>, ids: number[]): Map<number, number> {
	const next = new Map<number, number>();
	for (const id of ids) if (prev.has(id)) next.set(id, prev.get(id)!);
	const used = new Set(next.values());
	let free = 0;
	for (const id of ids) {
		if (next.has(id)) continue;
		while (used.has(free)) free++;
		next.set(id, free);
		used.add(free);
	}
	return next;
}

export const colorOf = (slots: Map<number, number>, batchId: number): string =>
	PALETTE[(slots.get(batchId) ?? 0) % PALETTE.length];

const line = (s: Series) => (s.smoothed.length > 0 ? s.smoothed : s.avg);

export function buildCompare(
	view: CompareView,
	slots: Map<number, number>,
	tempDisplay: TempDisplay,
): ComparePanel[] {
	const tables: { ts: number[]; cols: (number | null)[][] }[] = [];
	const columns: { panel: Panel; drawn: CompareDrawn }[] = [];

	const add = (
		panel: Panel,
		b: CompareView["batches"][number],
		s: Series,
		role: CompareDrawn["role"],
		values: (number | null)[],
	) => {
		tables.push({ ts: s.ts.map((ts) => toX(ts, "rel", b.pitched_at)), cols: [values] });
		columns.push({
			panel,
			drawn: { batchId: b.batch_id, code: b.code, color: colorOf(slots, b.batch_id), role },
		});
	};

	for (const b of view.batches) {
		const panel = b.y.kind === "fraction" ? "frac" : "grav";
		for (const s of b.gravity) {
			if (s.source_kind === "manual") add(panel, b, s, "manual", s.avg);
			else add(panel, b, s, "line", line(s));
		}
		if (b.temp) {
			const temp = line(b.temp).map((v) => (v === null ? null : toDisplayTemp(v, tempDisplay)));
			add("temp", b, b.temp, "line", temp);
		}
	}

	const [x, ...cols] = align(tables);
	return (["frac", "grav", "temp"] as const)
		.map((panel) => {
			const own = columns.flatMap((c, i) => (c.panel === panel ? [i] : []));
			return { panel, data: [x, ...own.map((i) => cols[i])], drawn: own.map((i) => columns[i].drawn) };
		})
		.filter((p) => p.drawn.length > 0);
}
