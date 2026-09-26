import { describe, expect, it } from "vitest";
import { csvName } from "./export";

const NOON = new Date(2026, 8, 26, 12).getTime() / 1000;

describe("csvName", () => {
	it("joins the parts and dates the file locally", () => {
		expect(csvName(["SELTZER-001", "readings"], NOON)).toBe("SELTZER-001 readings 2026-09-26.csv");
	});

	it("replaces characters a file name can't hold", () => {
		expect(csvName(['EC-1118 / 18 °C: "best"?'], NOON)).toBe(
			"EC-1118 - 18 °C- -best-- 2026-09-26.csv",
		);
	});

	it("skips empty parts", () => {
		expect(csvName(["", "  ", "query"], NOON)).toBe("query 2026-09-26.csv");
	});
});
