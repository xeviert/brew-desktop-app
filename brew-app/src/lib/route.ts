/**
 * Phase 4f. Hash routing in a few lines, no router dependency: a handful of screens and
 * their parameters don't earn one. Hash rather than path because Tauri serves a single file.
 */

export type Route =
	| { name: "home" }
	| { name: "batches" }
	/** `version` is a recipe_version id to start from, or `null` for a blank form. */
	| { name: "batch-new"; version: number | null }
	| { name: "batch"; id: number }
	| { name: "compare"; ids: number[] }
	| { name: "query"; id: number | null; preset?: QueryPreset }
	| { name: "ingredients"; tab: IngredientsTab }
	| { name: "recipes" }
	/** `version` is a `version_no`, not an id: `v3` is what a person reads. `null` is the newest. */
	| { name: "recipe"; id: number; version: number | null }
	| { name: "recipe-diff"; id: number; from: number; to: number }
	| { name: "settings" };

export type IngredientsTab = "ingredients" | "strains";

/** A fresh query started from somewhere else: U1's "used by N batches", U2's lock badge. */
export type QueryPreset = { lot: number } | { version: number };

/** Anything unrecognised is home: a stale bookmark should land somewhere useful. */
export function parse(hash: string): Route {
	const path = hash.replace(/^#/, "").replace(/\/+$/, "");
	if (path === "/settings") return { name: "settings" };
	if (path === "/recipes") return { name: "recipes" };
	// U3: `#/batches`, `#/batches/new`, `#/batches/new/4` (from recipe_version 4).
	if (path === "/batches") return { name: "batches" };
	const batchNew = /^\/batches\/new(?:\/(\d+))?$/.exec(path);
	if (batchNew) return { name: "batch-new", version: batchNew[1] ? Number(batchNew[1]) : null };
	const batch = /^\/batch\/(\d+)$/.exec(path);
	if (batch) return { name: "batch", id: Number(batch[1]) };
	// 5b: `#/compare/3,7,12`, in pick order; `#/compare` is an empty pick.
	const compare = /^\/compare(?:\/(\d+(?:,\d+)*))?$/.exec(path);
	if (compare) return { name: "compare", ids: compare[1] ? compare[1].split(",").map(Number) : [] };
	// U1/U2: `#/query/lot/12`, `#/query/version/4` — a fresh query preset to one of them.
	const preset = /^\/query\/(lot|version)\/(\d+)$/.exec(path);
	if (preset) {
		const id = Number(preset[2]);
		return { name: "query", id: null, preset: preset[1] === "lot" ? { lot: id } : { version: id } };
	}
	// 5c: `#/query` is a fresh filter, `#/query/4` saved query 4.
	const query = /^\/query(?:\/(\d+))?$/.exec(path);
	if (query) return { name: "query", id: query[1] ? Number(query[1]) : null };
	// U1: `#/ingredients`, `#/ingredients/strains`.
	const ingredients = /^\/ingredients(?:\/(strains))?$/.exec(path);
	if (ingredients) return { name: "ingredients", tab: ingredients[1] ? "strains" : "ingredients" };
	// U2: `#/recipe/3`, `#/recipe/3/v2`, `#/recipe/3/diff/1,2`.
	const recipe = /^\/recipe\/(\d+)(?:\/v(\d+))?$/.exec(path);
	if (recipe) return { name: "recipe", id: Number(recipe[1]), version: recipe[2] ? Number(recipe[2]) : null };
	const diff = /^\/recipe\/(\d+)\/diff\/(\d+),(\d+)$/.exec(path);
	if (diff) return { name: "recipe-diff", id: Number(diff[1]), from: Number(diff[2]), to: Number(diff[3]) };
	return { name: "home" };
}

export function href(route: Route): string {
	switch (route.name) {
		case "home":
			return "#/";
		case "batches":
			return "#/batches";
		case "batch-new":
			return route.version === null ? "#/batches/new" : `#/batches/new/${route.version}`;
		case "batch":
			return `#/batch/${route.id}`;
		case "compare":
			return route.ids.length ? `#/compare/${route.ids.join(",")}` : "#/compare";
		case "query":
			if (route.id === null && route.preset) {
				return "lot" in route.preset ? `#/query/lot/${route.preset.lot}` : `#/query/version/${route.preset.version}`;
			}
			return route.id === null ? "#/query" : `#/query/${route.id}`;
		case "ingredients":
			return route.tab === "strains" ? "#/ingredients/strains" : "#/ingredients";
		case "recipes":
			return "#/recipes";
		case "recipe":
			return route.version === null ? `#/recipe/${route.id}` : `#/recipe/${route.id}/v${route.version}`;
		case "recipe-diff":
			return `#/recipe/${route.id}/diff/${route.from},${route.to}`;
		case "settings":
			return "#/settings";
	}
}
