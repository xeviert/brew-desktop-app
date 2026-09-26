import { describe, expect, it } from "vitest";
import type { QueryRow } from "./api";
import {
	emptyFilter,
	formatSpread,
	preselect,
	rollupLabel,
	sortRows,
	toFilter,
	toForm,
} from "./query";

const row = (over: Partial<QueryRow>): QueryRow => ({
	batch_id: 1,
	code: "B1",
	beverage_type: "seltzer",
	status: "packaged",
	archived_at: null,
	pitched_at: 0,
	og: null,
	fg: null,
	lag: { missing: "no_og" },
	peak_rate: { missing: "no_sensor_gravity" },
	terminal: { missing: "no_sensor_gravity" },
	apparent_attenuation: { missing: "no_og" },
	abv_alternate: { missing: "no_og" },
	ferm_temp: { missing: "no_temp" },
	yan_added_mg_per_l: { missing: "no_volume" },
	cost_per_litre: { missing: "no_volume" },
	cost_lines_without_cost: 0,
	strains: [],
	display_og: null,
	display_fg: null,
	...over,
});

describe("toFilter", () => {
	const blank = toForm(emptyFilter(), "C");

	it("leaves blank ends open and an all-blank range off", () => {
		const got = toFilter({ ...blank, og_min: "1.035", og_max: " " }, "C");
		expect(got).toEqual({ ok: true, filter: { ...emptyFilter(), og: { min: 1.035, max: null } } });
	});
	it("stores temperature in °C whatever it was typed in", () => {
		const got = toFilter({ ...blank, temp_min: "64.4" }, "F");
		if (!got.ok) throw new Error(got.error);
		expect(got.filter.ferm_temp_c?.min).toBeCloseTo(18);
	});
	it("takes a pitch range as whole local days", () => {
		const got = toFilter({ ...blank, pitched_from: "2026-03-01", pitched_to: "2026-03-01" }, "C");
		if (!got.ok) throw new Error(got.error);
		const { min, max } = got.filter.pitched!;
		expect(new Date(min! * 1000)).toEqual(new Date(2026, 2, 1));
		expect(new Date((max! + 1) * 1000)).toEqual(new Date(2026, 2, 2));
	});
	it("refuses an end it can't read rather than dropping it", () => {
		expect(toFilter({ ...blank, fg_max: "1.o10" }, "C")).toEqual({
			ok: false,
			error: "Not readable: FG max",
		});
	});
	it("round-trips through the form in either unit", () => {
		const filter = {
			...emptyFilter(),
			strain_ids: [3],
			og: { min: 1.035, max: 1.04 },
			ferm_temp_c: { min: 17.5, max: null },
			pitched: {
				min: new Date(2026, 0, 5).getTime() / 1000,
				max: new Date(2026, 0, 8).getTime() / 1000 - 1,
			},
		};
		for (const temp of ["C", "F"] as const) {
			const back = toFilter(toForm(filter, temp), temp);
			if (!back.ok) throw new Error(back.error);
			expect(back.filter.og).toEqual(filter.og);
			expect(back.filter.ferm_temp_c?.min).toBeCloseTo(17.5);
			expect(back.filter.pitched).toEqual(filter.pitched);
			expect(back.filter.strain_ids).toEqual([3]);
		}
	});
});

describe("sortRows", () => {
	const rows = [
		row({ batch_id: 1, code: "A", lag: { value: { at: 0, secs: 7200 } } }),
		row({ batch_id: 2, code: "B" }),
		row({ batch_id: 3, code: "C", lag: { value: { at: 0, secs: 3600 } } }),
	];
	it("puts missing values last in either direction", () => {
		expect(sortRows(rows, "lag", false).map((r) => r.code)).toEqual(["C", "A", "B"]);
		expect(sortRows(rows, "lag", true).map((r) => r.code)).toEqual(["A", "C", "B"]);
	});
	it("sorts codes as text", () => {
		expect(sortRows(rows, "code", true).map((r) => r.code)).toEqual(["C", "B", "A"]);
	});
});

describe("preselect", () => {
	it("ticks the pitched batches of a small result", () => {
		const rows = [row({ batch_id: 1 }), row({ batch_id: 2, pitched_at: null }), row({ batch_id: 3 })];
		expect(preselect(rows)).toEqual([1, 3]);
	});
	it("ticks nothing past the overlay limit", () => {
		expect(preselect(Array.from({ length: 9 }, (_, i) => row({ batch_id: i })))).toEqual([]);
	});
});

describe("formatSpread", () => {
	const pct = (v: number) => `${Math.round(v * 100)} %`;

	it("shows the median with its range", () => {
		expect(formatSpread({ median: 0.98, min: 0.9, max: 1.02, n: 3 }, pct)).toBe("98 % (90 %–102 %)");
	});

	it("drops a range of one value", () => {
		expect(formatSpread({ median: 0.98, min: 0.98, max: 0.98, n: 1 }, pct)).toBe("98 %");
	});

	it("is a dash when no batch had it", () => {
		expect(formatSpread(null, pct)).toBe("—");
	});
});

describe("rollupLabel", () => {
	it("names a strain, or every strain of a mixed row", () => {
		expect(rollupLabel({ kind: "strain", id: 1, name: "EC-1118" })).toBe("EC-1118");
		expect(
			rollupLabel({
				kind: "mixed",
				strains: [
					{ id: 1, name: "EC-1118" },
					{ id: 2, name: "US-05" },
				],
			}),
		).toBe("Mixed: EC-1118 + US-05");
	});
});
