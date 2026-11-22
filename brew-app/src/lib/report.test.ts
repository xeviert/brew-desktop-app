import { describe, expect, it } from "vitest";
import type { Cleaning, Curve, Series } from "./api";
import { fermentationRows, hygieneAtPitch, reportText, sampleLabel, sampleTimes, valueAt, type Report } from "./report";

const PITCH = 1_700_000_000;
const HOUR = 3_600;
const DAY = 24 * HOUR;

function series(over: Partial<Series> = {}): Series {
	return {
		stream_id: 1,
		source_id: 1,
		metric: "sg",
		source_kind: "brewd_journal",
		label: "iSpindel",
		bucket_secs: null,
		ts: [],
		avg: [],
		min: [],
		max: [],
		smoothed: [],
		smoothed_partial: [],
		...over,
	};
}

describe("sampleTimes", () => {
	it("is the pitch, 12 h, then daily", () => {
		expect(sampleTimes(PITCH, PITCH + 2 * DAY).map((t) => sampleLabel(t, PITCH))).toEqual([
			"0 h",
			"12 h",
			"Day 1",
			"Day 2",
		]);
	});

	it("goes weekly after four weeks", () => {
		const labels = sampleTimes(PITCH, PITCH + 42 * DAY).map((t) => sampleLabel(t, PITCH));
		expect(labels.slice(-3)).toEqual(["Day 28", "Day 35", "Day 42"]);
	});

	it("is nothing before the pitch", () => {
		expect(sampleTimes(PITCH, PITCH - 1)).toEqual([]);
	});
});

describe("valueAt", () => {
	const sensor = series({ ts: [PITCH, PITCH + HOUR], avg: [1.04, 1.039], smoothed: [1.0405, 1.0395] });
	const manual = series({ source_kind: "manual", ts: [PITCH + 10 * DAY], avg: [1.002] });

	it("prefers the smoothed line, nearest in the window", () => {
		expect(valueAt([sensor], PITCH + 50 * 60, HOUR)).toBe(1.0395);
	});

	it("falls back to the next series, and to nothing outside every window", () => {
		expect(valueAt([sensor, manual], PITCH + 10 * DAY, 6 * HOUR)).toBe(1.002);
		expect(valueAt([sensor, manual], PITCH + 5 * DAY, 6 * HOUR)).toBeNull();
	});
});

describe("fermentationRows", () => {
	it("reads gravity, vessel temperature and pH at each sample, dropping empty ones", () => {
		const curve: Curve = {
			range: { start: PITCH, end: PITCH + 3 * DAY },
			gravity_unit: "SG",
			series: [
				series({ ts: [PITCH, PITCH + DAY], avg: [1.037, 1.021] }),
				series({ stream_id: 2, metric: "temp_c", ts: [PITCH, PITCH + DAY], avg: [18.1, 18.0] }),
				series({ stream_id: 3, metric: "ph", source_kind: "manual", ts: [PITCH + 2 * HOUR], avg: [3.42] }),
			],
		};
		expect(fermentationRows(curve, PITCH, PITCH + 3 * DAY, "C")).toEqual([
			{ at: "0 h", gravity: "1.037", temp: "18.1 °C", ph: "pH 3.42" },
			{ at: "12 h", gravity: null, temp: null, ph: "pH 3.42" },
			{ at: "Day 1", gravity: "1.021", temp: "18.0 °C", ph: null },
		]);
	});
});

describe("hygieneAtPitch", () => {
	const entry = (over: Partial<Cleaning>): Cleaning => ({
		id: 1,
		vessel_id: 1,
		cleaned_at: PITCH - 3 * HOUR,
		kind: "sanitize",
		chemical: "Star San",
		notes: null,
		...over,
	});

	it("is the newest sanitise before the pitch", () => {
		const entries = [entry({ cleaned_at: PITCH - 5 * DAY }), entry({}), entry({ cleaned_at: PITCH + HOUR, chemical: null })];
		expect(hygieneAtPitch(entries, PITCH, "FV1")).toBe("FV1: sanitised 3 h before the pitch (Star San)");
	});

	it("says when there was none, and nothing before a pitch", () => {
		expect(hygieneAtPitch([entry({ kind: "clean" })], PITCH, "FV1")).toBe("FV1: no sanitise logged before the pitch");
		expect(hygieneAtPitch([], null, "FV1")).toBeNull();
	});
});

describe("reportText", () => {
	it("is lines under headings, skipping empty sections", () => {
		const report: Report = {
			code: "SELTZER-024",
			facts: [
				{ label: "Recipe", value: "Citron-Verveine v3" },
				{ label: "OG", value: "1.037" },
			],
			fermentation: [{ at: "0 h", gravity: "1.037", temp: "18.1 °C", ph: null }],
			gravityUnit: "SG",
			ingredients: [{ stage: "prep", text: "Sucrose, 4 kg (Metro)", cost: "€10.00" }],
			cost: "€10.00",
			events: [],
			packaging: [],
			packagingSummary: null,
			hygiene: "FV1: sanitised 3 h before the pitch",
		};
		expect(reportText(report)).toBe(
			[
				"Batch SELTZER-024",
				"",
				"Recipe: Citron-Verveine v3",
				"OG: 1.037",
				"FV1: sanitised 3 h before the pitch",
				"",
				"Fermentation:",
				"0 h: 1.037, 18.1 °C",
				"",
				"Ingredients:",
				"prep: Sucrose, 4 kg (Metro), €10.00",
				"Cost: €10.00",
			].join("\n"),
		);
	});
});
