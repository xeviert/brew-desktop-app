import { describe, expect, it } from "vitest";
import type { ComparedBatch, CompareView, Series } from "./api";
import { PALETTE, assignSlots, buildCompare } from "./compare";

const series = (over: Partial<Series>): Series => ({
	stream_id: 1,
	source_id: 1,
	metric: "sg",
	source_kind: "brewd_journal",
	label: "hydrometer",
	bucket_secs: null,
	ts: [],
	avg: [],
	min: [],
	max: [],
	smoothed: [],
	smoothed_partial: [],
	...over,
});

const batch = (over: Partial<ComparedBatch>): ComparedBatch => ({
	batch_id: 1,
	code: "B1",
	pitched_at: 0,
	og: 1.05,
	y: { kind: "fraction" },
	gravity: [],
	temp: null,
	...over,
});

const view = (...batches: ComparedBatch[]): CompareView => ({
	span: { start: 0, end: 7_200 },
	batches,
	excluded: [],
	gravity_unit: "SG",
});

describe("assignSlots", () => {
	it("keeps a batch's colour when another is removed", () => {
		const first = assignSlots(new Map(), [10, 20, 30]);
		expect([...first.values()]).toEqual([0, 1, 2]);
		const after = assignSlots(first, [10, 30]);
		expect(after.get(30)).toBe(2);
		// A newcomer fills the gap rather than taking a fourth colour.
		expect(assignSlots(after, [10, 30, 40]).get(40)).toBe(1);
	});
});

describe("buildCompare", () => {
	it("puts different pitches on the same hours-since-pitch axis", () => {
		const a = batch({
			batch_id: 1,
			pitched_at: 1_000,
			gravity: [series({ ts: [1_000, 4_600], smoothed: [0, 0.5] })],
		});
		const b = batch({
			batch_id: 2,
			pitched_at: 90_000,
			gravity: [series({ ts: [90_000, 93_600], smoothed: [0, 0.4] })],
		});
		const [frac] = buildCompare(view(a, b), assignSlots(new Map(), [1, 2]), "C");
		expect(frac.panel).toBe("frac");
		expect(frac.data).toEqual([
			[0, 1],
			[0, 0.5],
			[0, 0.4],
		]);
		expect(frac.drawn.map((d) => d.color)).toEqual([PALETTE[0], PALETTE[1]]);
	});

	it("draws a batch left raw on its own gravity panel", () => {
		const a = batch({ batch_id: 1, gravity: [series({ ts: [0], smoothed: [0.1] })] });
		const b = batch({
			batch_id: 2,
			og: null,
			y: { kind: "raw", reason: "no_og" },
			gravity: [series({ ts: [0], avg: [1.04] }), series({ source_kind: "manual", ts: [0], avg: [1.041] })],
		});
		const panels = buildCompare(view(a, b), assignSlots(new Map(), [1, 2]), "C");
		expect(panels.map((p) => p.panel)).toEqual(["frac", "grav"]);
		expect(panels[1].drawn.map((d) => d.role)).toEqual(["line", "manual"]);
		expect(panels[1].data.slice(1)).toEqual([[1.04], [1.041]]);
		// Every panel shares one x so the synced cursors land on the same hour.
		expect(panels[0].data[0]).toBe(panels[1].data[0]);
	});

	it("converts temperature to the display unit", () => {
		const a = batch({ temp: series({ metric: "temp_c", ts: [0, 3_600], avg: [20, null] }) });
		const temp = buildCompare(view(a), new Map([[1, 0]]), "F").find((p) => p.panel === "temp")!;
		expect(temp.data[1]).toEqual([68, null]);
	});
});
