import { describe, expect, it } from "vitest";
import { href, parse, type Route } from "./route";

describe("parse", () => {
	it("reads the three screens", () => {
		expect(parse("#/")).toEqual({ name: "home" });
		expect(parse("#/batch/17")).toEqual({ name: "batch", id: 17 });
		expect(parse("#/settings")).toEqual({ name: "settings" });
	});
	it("reads a comparison in pick order", () => {
		expect(parse("#/compare/7,3,12")).toEqual({ name: "compare", ids: [7, 3, 12] });
		expect(parse("#/compare")).toEqual({ name: "compare", ids: [] });
		expect(parse("#/compare/3,")).toEqual({ name: "home" });
	});
	it("reads a fresh or a saved query", () => {
		expect(parse("#/query")).toEqual({ name: "query", id: null });
		expect(parse("#/query/4")).toEqual({ name: "query", id: 4 });
		expect(parse("#/query/x")).toEqual({ name: "home" });
	});
	it("reads a query preset to one lot", () => {
		expect(parse("#/query/lot/12")).toEqual({ name: "query", id: null, lot: 12 });
		expect(parse("#/query/lot")).toEqual({ name: "home" });
	});
	it("reads the two ingredient tabs", () => {
		expect(parse("#/ingredients")).toEqual({ name: "ingredients", tab: "ingredients" });
		expect(parse("#/ingredients/strains")).toEqual({ name: "ingredients", tab: "strains" });
		expect(parse("#/ingredients/lots")).toEqual({ name: "home" });
	});
	it("treats an empty hash and a trailing slash as the same screen", () => {
		expect(parse("")).toEqual({ name: "home" });
		expect(parse("#/settings/")).toEqual({ name: "settings" });
	});
	it("sends anything else home", () => {
		expect(parse("#/batch/abc")).toEqual({ name: "home" });
		expect(parse("#/nowhere")).toEqual({ name: "home" });
	});
	it("round-trips through href", () => {
		const routes: Route[] = [
			{ name: "home" },
			{ name: "batch", id: 3 },
			{ name: "compare", ids: [] },
			{ name: "compare", ids: [4, 2] },
			{ name: "query", id: null },
			{ name: "query", id: 9 },
			{ name: "query", id: null, lot: 5 },
			{ name: "ingredients", tab: "ingredients" },
			{ name: "ingredients", tab: "strains" },
			{ name: "settings" },
		];
		for (const r of routes) {
			expect(parse(href(r))).toEqual(r);
		}
	});
});
