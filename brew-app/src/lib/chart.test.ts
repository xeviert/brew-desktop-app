import { describe, expect, it } from "vitest";
import type { Curve, Series } from "./api";
import { align, axisOf, buildPlot, latest, toAbs, toX } from "./chart";

const series = (over: Partial<Series>): Series => ({
	stream_id: 1,
	source_id: 1,
	metric: "temp_c",
	source_kind: "brewd_journal",
	label: "probe",
	bucket_secs: null,
	ts: [],
	avg: [],
	min: [],
	max: [],
	smoothed: [],
	smoothed_partial: [],
	...over,
});

const curve = (...s: Series[]): Curve => ({
	range: { start: 0, end: 10_000 },
	series: s,
	gravity_unit: "SG",
});

describe("toX / toAbs", () => {
	it("round-trips in both modes", () => {
		const pitch = 1_700_000_000;
		for (const mode of ["abs", "rel"] as const) {
			const ts = pitch + 5_400;
			expect(toAbs(toX(ts, mode, pitch), mode, pitch)).toBe(ts);
		}
		expect(toX(pitch + 5_400, "rel", pitch)).toBe(1.5);
		expect(toX(pitch, "rel", pitch)).toBe(0);
	});

	it("falls back to absolute time with no pitch", () => {
		expect(toX(42, "rel", null)).toBe(42);
		expect(toAbs(42, "rel", null)).toBe(42);
	});
});

describe("align", () => {
	it("keeps a server gap as null and fills misalignment with undefined", () => {
		const data = align([
			{ ts: [0, 10, 20], cols: [[1, null, 3]] },
			{ ts: [5, 20], cols: [[7, 8]] },
		]);
		expect(data[0]).toEqual([0, 5, 10, 20]);
		expect(data[1]).toEqual([1, undefined, null, 3]);
		expect(data[2]).toEqual([undefined, 7, undefined, 8]);
	});
});

describe("buildPlot", () => {
	it("keeps device health off the chart", () => {
		for (const m of ["battery_v", "rssi_dbm", "tilt_deg", "state"]) expect(axisOf(m)).toBeNull();
		const plot = buildPlot(
			curve(series({ metric: "battery_v", ts: [0], avg: [4.1] })),
			"abs",
			null,
			"C",
		);
		expect(plot.drawn).toEqual([]);
		expect(plot.axes).toEqual([]);
	});

	it("draws smoothed gravity as the line with raw behind and a band when bucketed", () => {
		const plot = buildPlot(
			curve(
				series({
					metric: "sg",
					bucket_secs: 60,
					ts: [0, 60],
					avg: [1.04, 1.039],
					min: [1.039, 1.038],
					max: [1.052, 1.04],
					smoothed: [1.04, 1.039],
					smoothed_partial: [true, false],
				}),
			),
			"abs",
			null,
			"C",
		);
		expect(plot.drawn.map((d) => d.role)).toEqual(["smoothed", "raw", "hi", "lo"]);
		expect(plot.bands).toEqual([[3, 4]]);
		expect(plot.data[3]).toEqual([1.052, 1.04]);
	});

	it("never bands a manual series", () => {
		const plot = buildPlot(
			curve(series({ metric: "ph", source_kind: "manual", ts: [0], avg: [3.2] })),
			"abs",
			null,
			"C",
		);
		expect(plot.drawn.map((d) => d.role)).toEqual(["manual"]);
		expect(plot.bands).toEqual([]);
	});

	it("shows the pH axis only when there is a pH reading", () => {
		const temp = series({ ts: [0], avg: [18] });
		const ph = series({ stream_id: 2, metric: "ph", source_kind: "manual" });
		expect(buildPlot(curve(temp, ph), "abs", null, "C").axes).toEqual(["temp"]);
		const read = { ...ph, ts: [0], avg: [3.4] };
		expect(buildPlot(curve(temp, read), "abs", null, "C").axes).toEqual(["temp", "ph"]);
	});

	it("converts temperature and keeps its gaps", () => {
		const plot = buildPlot(curve(series({ ts: [0, 30], avg: [100, null] })), "abs", null, "F");
		expect(plot.data[1]).toEqual([212, null]);
	});

	it("puts x in hours since pitch", () => {
		const plot = buildPlot(curve(series({ ts: [3_600, 7_200], avg: [18, 18] })), "rel", 0, "C");
		expect(plot.data[0]).toEqual([1, 2]);
	});
});

describe("latest", () => {
	it("takes each column's newest value, skipping gaps and alignment holes", () => {
		const data = [
			[10, 20, 30],
			[1, 2, undefined],
			[5, null, null],
		];
		expect(latest(data)).toEqual([20, 2, 5]);
	});

	it("is all null with no data", () => {
		expect(latest([[], []])).toEqual([null, null]);
		expect(latest([[1], [null]])).toEqual([null, null]);
	});
});
