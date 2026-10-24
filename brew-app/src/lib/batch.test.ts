import { describe, expect, it } from "vitest";
import type { BatchSummary } from "./api";
import {
	byStatus,
	editForm,
	fromLocalDateTime,
	headerFacts,
	localDateTime,
	recipeLabel,
	toBatchEdit,
	toBatchInput,
	type BatchForm,
} from "./batch";
import { formatTimestamp } from "./units";

const summary = (over: Partial<BatchSummary>): BatchSummary => ({
	id: 1,
	code: "SELTZER-001",
	recipe_version_id: 4,
	beverage_type: "seltzer",
	vessel_id: 2,
	status: "planned",
	volume_l: 20,
	og: null,
	fg: null,
	pitched_at: null,
	archived_at: null,
	notes: null,
	recipe_id: 3,
	recipe_name: "Citron-Verveine",
	version_no: 2,
	vessel_name: "FV1",
	...over,
});

const form: BatchForm = {
	code: " SELTZER-001 ",
	recipe_version_id: 4,
	beverage_type: "seltzer",
	vessel_id: null,
	volume: "20",
	og: "",
	notes: "",
};

describe("datetime-local", () => {
	it("round-trips a local minute", () => {
		const at = new Date(2026, 9, 24, 18, 5).getTime() / 1000;
		expect(localDateTime(at)).toBe("2026-10-24T18:05");
		expect(fromLocalDateTime("2026-10-24T18:05")).toBe(at);
	});
	it("accepts seconds and refuses anything else", () => {
		expect(fromLocalDateTime("2026-10-24T18:05:30")).toBe(new Date(2026, 9, 24, 18, 5, 30).getTime() / 1000);
		expect(fromLocalDateTime("")).toBeNull();
		expect(fromLocalDateTime("2026-10-24")).toBeNull();
	});
});

describe("create", () => {
	it("trims the code, reads a decimal comma, and leaves blanks null", () => {
		expect(toBatchInput({ ...form, og: "10,5" })).toEqual({
			ok: true,
			value: {
				code: "SELTZER-001",
				recipe_version_id: 4,
				beverage_type: "seltzer",
				vessel_id: null,
				volume_l: 20,
				og: 10.5,
				notes: null,
			},
		});
	});
	it("names every unreadable field, and needs a code", () => {
		expect(toBatchInput({ ...form, volume: "twenty", og: "1.0x" })).toEqual({
			ok: false,
			error: "Not readable: volume, OG",
		});
		expect(toBatchInput({ ...form, code: "  " }).ok).toBe(false);
	});
});

describe("edit", () => {
	it("round-trips a batch, and a cleared volume is sent as null", () => {
		const f = editForm(summary({ notes: "dry" }));
		expect(toBatchEdit(f)).toEqual({
			ok: true,
			value: { code: "SELTZER-001", vessel_id: 2, volume_l: 20, notes: "dry" },
		});
		expect(toBatchEdit({ ...f, volume: "" })).toMatchObject({ ok: true, value: { volume_l: null } });
	});
});

describe("list", () => {
	it("labels the recipe version, or its absence", () => {
		expect(recipeLabel(summary({}))).toBe("Citron-Verveine v2");
		expect(recipeLabel(summary({ recipe_id: null, recipe_name: null, version_no: null }))).toBe("no recipe");
	});
	it("groups by status in process order", () => {
		const groups = byStatus([
			summary({ id: 1, status: "packaged" }),
			summary({ id: 2, status: "planned" }),
			summary({ id: 3, status: "packaged" }),
		]);
		expect(groups.map((g) => [g.status, g.items.map((b) => b.id)])).toEqual([
			["planned", [2]],
			["packaged", [1, 3]],
		]);
	});
});

describe("screen", () => {
	const pitched = 1_790_000_000;
	it("names the vessel and volume, and the day while in the vessel", () => {
		expect(headerFacts(summary({ volume_l: 19.456 }))).toEqual(["FV1", "19.46 L"]);
		const fermenting = summary({ status: "fermenting", pitched_at: pitched });
		expect(headerFacts(fermenting, pitched + 3_600)).toEqual(["FV1", "20 L", "day 1", `pitched ${formatTimestamp(pitched)}`]);
		expect(headerFacts(fermenting, pitched + 2 * 86_400 + 1)[2]).toBe("day 3");
	});
	it("counts no days once packaged or archived, and leaves out what is not set", () => {
		const after = pitched + 30 * 86_400;
		expect(headerFacts(summary({ status: "packaged", pitched_at: pitched }), after)).toEqual([
			"FV1",
			"20 L",
			`pitched ${formatTimestamp(pitched)}`,
		]);
		expect(
			headerFacts(summary({ status: "fermenting", pitched_at: pitched, archived_at: after }), after),
		).not.toContain("day 31");
		expect(headerFacts(summary({ vessel_id: null, vessel_name: null, volume_l: null }))).toEqual([]);
	});
});
