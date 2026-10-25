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
	it("reads a query preset to one lot or one recipe version", () => {
		expect(parse("#/query/lot/12")).toEqual({ name: "query", id: null, preset: { lot: 12 } });
		expect(parse("#/query/version/4")).toEqual({ name: "query", id: null, preset: { version: 4 } });
		expect(parse("#/query/lot")).toEqual({ name: "home" });
	});
	it("reads recipes, a version by its number, and a diff", () => {
		expect(parse("#/recipes")).toEqual({ name: "recipes" });
		expect(parse("#/recipe/3")).toEqual({ name: "recipe", id: 3, version: null });
		expect(parse("#/recipe/3/v2")).toEqual({ name: "recipe", id: 3, version: 2 });
		expect(parse("#/recipe/3/diff/1,2")).toEqual({ name: "recipe-diff", id: 3, from: 1, to: 2 });
		expect(parse("#/recipe/3/v")).toEqual({ name: "home" });
		expect(parse("#/recipe/3/diff/1")).toEqual({ name: "home" });
	});
	it("reads the batch list and a new batch, blank or from a version", () => {
		expect(parse("#/batches")).toEqual({ name: "batches" });
		expect(parse("#/batches/new")).toEqual({ name: "batch-new", version: null });
		expect(parse("#/batches/new/4")).toEqual({ name: "batch-new", version: 4 });
		expect(parse("#/batches/new/x")).toEqual({ name: "home" });
	});
	it("reads the two ingredient tabs", () => {
		expect(parse("#/ingredients")).toEqual({ name: "ingredients", tab: "ingredients" });
		expect(parse("#/ingredients/strains")).toEqual({ name: "ingredients", tab: "strains" });
		expect(parse("#/ingredients/lots")).toEqual({ name: "home" });
	});
	it("reads the calculators, blank or from a version", () => {
		expect(parse("#/calc")).toEqual({ name: "calc", version: null });
		expect(parse("#/calc/version/4")).toEqual({ name: "calc", version: 4 });
		expect(parse("#/calc/4")).toEqual({ name: "home" });
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
			{ name: "batches" },
			{ name: "batch-new", version: null },
			{ name: "batch-new", version: 4 },
			{ name: "compare", ids: [] },
			{ name: "compare", ids: [4, 2] },
			{ name: "query", id: null },
			{ name: "query", id: 9 },
			{ name: "query", id: null, preset: { lot: 5 } },
			{ name: "query", id: null, preset: { version: 6 } },
			{ name: "recipes" },
			{ name: "recipe", id: 3, version: null },
			{ name: "recipe", id: 3, version: 2 },
			{ name: "recipe-diff", id: 3, from: 1, to: 2 },
			{ name: "ingredients", tab: "ingredients" },
			{ name: "ingredients", tab: "strains" },
			{ name: "calc", version: null },
			{ name: "calc", version: 4 },
			{ name: "settings" },
		];
		for (const r of routes) {
			expect(parse(href(r))).toEqual(r);
		}
	});
});
