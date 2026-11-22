import { describe, expect, it } from "vitest";
import type { Config, Measurement, TimelineItem } from "./api";
import {
	entryDetail,
	eventForm,
	formatValue,
	moments,
	quickForm,
	sincePitch,
	toEventInput,
	toQuickEntry,
	type QuickForm,
} from "./timeline";

const config = {
	ui: { temp_display: "C", gravity_display: "sg" },
	measurement: { hydrometer_calibration_c: 20, refractometer_wort_correction: 1 },
} as Config;

const AT = new Date(2026, 9, 24, 18, 5).getTime() / 1000;
const blank: QuickForm = quickForm(config, AT);

const measurement = (over: Partial<Measurement & { display: number | null }>): Measurement & { display: number | null } => ({
	id: 1,
	batch_id: 1,
	stream_id: 1,
	ts: AT,
	metric: "sg",
	value: 1.0112,
	entry_value: 1.01,
	entry_unit: "sg",
	instrument: "hydrometer",
	sample_temp_c: null,
	calibration_temp_c: null,
	wort_correction: null,
	display: 1.0112,
	...over,
});

describe("quick entry", () => {
	it("starts at now with the config's correction defaults, in the display units", () => {
		expect(blank.at).toBe("2026-10-24T18:05");
		expect([blank.scale, blank.calibrationTemp, blank.wortCorrection]).toEqual(["sg", "20", "1"]);
		const f = quickForm({ ...config, ui: { temp_display: "F", gravity_display: "plato" } }, AT);
		expect([f.scale, f.calibrationTemp]).toEqual(["plato", "68"]);
	});

	it("sends every filled reading at one time, a hydrometer corrected only when a sample temp is typed", () => {
		const r = toQuickEntry({ ...blank, gravity: "1,010", ph: "3.3", ta: "5", note: " dry " }, "C");
		expect(r).toEqual({
			ok: true,
			value: {
				ts: AT,
				note: "dry",
				measurements: [
					{
						metric: "sg",
						entry_value: 1.01,
						entry_unit: "sg",
						instrument: "hydrometer",
						sample_temp_c: null,
						calibration_temp_c: null,
						wort_correction: null,
					},
					expect.objectContaining({ metric: "ph", entry_unit: "ph", instrument: "ph_meter" }),
					expect.objectContaining({ metric: "ta", entry_unit: "g_per_l", entry_value: 5 }),
				],
			},
		});
	});

	it("converts the hydrometer temperatures from the display scale, and types a temperature in it", () => {
		const r = toQuickEntry({ ...blank, gravity: "1.010", sampleTemp: "77", calibrationTemp: "68", temp: "64.4" }, "F");
		if (!r.ok) throw new Error(r.error);
		const [g, t] = r.value.measurements;
		expect(g.sample_temp_c).toBeCloseTo(25);
		expect(g.calibration_temp_c).toBeCloseTo(20);
		expect([t.metric, t.entry_unit, t.entry_value]).toEqual(["temp_c", "f", 64.4]);
	});

	it("sends a refractometer as Brix with its wort factor and no temperatures", () => {
		const r = toQuickEntry({ ...blank, instrument: "refractometer", scale: "sg", gravity: "6.2", sampleTemp: "25" }, "C");
		if (!r.ok) throw new Error(r.error);
		expect(r.value.measurements[0]).toMatchObject({
			entry_unit: "brix",
			instrument: "refractometer",
			wort_correction: 1,
			sample_temp_c: null,
			calibration_temp_c: null,
		});
	});

	it("refuses nothing at all, an unreadable field, and a sample temperature with no calibration", () => {
		expect(toQuickEntry(blank, "C")).toEqual({ ok: false, error: "Nothing to record" });
		expect(toQuickEntry({ ...blank, ph: "three", at: "" }, "C")).toEqual({
			ok: false,
			error: "Not readable: time, pH",
		});
		expect(toQuickEntry({ ...blank, gravity: "1.010", sampleTemp: "25", calibrationTemp: "" }, "C").ok).toBe(false);
	});

	it("accepts a note on its own", () => {
		expect(toQuickEntry({ ...blank, note: "smells of sulphur" }, "C")).toMatchObject({
			ok: true,
			value: { measurements: [], note: "smells of sulphur" },
		});
	});
});

describe("events", () => {
	it("round-trips an event, and a new one is a note at now", () => {
		expect(eventForm(null, AT)).toEqual({ at: "2026-10-24T18:05", kind: "note", note: "" });
		const f = eventForm({ ts: AT, kind: "cold_crash", note: "2 °C" }, 0);
		expect(toEventInput(f)).toEqual({ ok: true, value: { ts: AT, kind: "cold_crash", note: "2 °C" } });
		expect(toEventInput({ ...f, at: "" }).ok).toBe(false);
	});
});

describe("the list", () => {
	it("groups by timestamp, newest first, readings apart from events", () => {
		const items: TimelineItem[] = [
			{ type: "event", id: 1, batch_id: 1, ts: 100, kind: "pitch", payload: null, note: null },
			{ type: "measurement", ...measurement({ id: 2, ts: 200 }) },
			{ type: "measurement", ...measurement({ id: 3, ts: 200, metric: "ph", value: 3.3, display: null }) },
			{ type: "event", id: 4, batch_id: 1, ts: 200, kind: "note", payload: null, note: "dry" },
		];
		const m = moments(items);
		expect(m.map((x) => [x.ts, x.measurements.map((i) => i.id), x.events.map((i) => i.id)])).toEqual([
			[200, [2, 3], [4]],
			[100, [], [1]],
		]);
	});

	it("places a moment against the pitch", () => {
		expect(sincePitch(1_000 + 26 * 3_600, 1_000)).toBe("t+26 h");
		expect(sincePitch(1_000 + 76 * 3_600, 1_000)).toBe("t+3 d 4 h");
		expect(sincePitch(1_000 - 2 * 3_600, 1_000)).toBe("t−2 h");
		expect(sincePitch(1_000, null)).toBe("");
	});

	it("says a value in the display units and what was typed", () => {
		expect(formatValue(measurement({}), "SG", "C")).toBe("1.0112");
		expect(formatValue(measurement({ display: 2.8 }), "°P", "C")).toBe("2.8 °P");
		expect(formatValue(measurement({ metric: "temp_c", value: 18, display: null }), "SG", "F")).toBe("64.4 °F");
		expect(entryDetail(measurement({}), "C")).toBe("hydrometer");
		expect(entryDetail(measurement({ entry_unit: "plato", entry_value: 2.6 }), "C")).toBe("hydrometer 2.6 °P");
		expect(entryDetail(measurement({ sample_temp_c: 25, calibration_temp_c: 20 }), "C")).toBe(
			"hydrometer 1.0100 at 25.0 °C, corrected to 20 °C",
		);
		expect(
			entryDetail(measurement({ instrument: "refractometer", entry_unit: "brix", entry_value: 6.2, wort_correction: 1.04 }), "C"),
		).toBe("refractometer 6.2 °Bx, wort factor 1.04");
	});
});
