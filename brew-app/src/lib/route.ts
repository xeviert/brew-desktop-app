/**
 * Phase 4f. Hash routing in a few lines, no router dependency: a handful of screens and
 * their parameters don't earn one. Hash rather than path because Tauri serves a single file.
 */

export type Route =
	| { name: "home" }
	| { name: "batch"; id: number }
	| { name: "compare"; ids: number[] }
	| { name: "query"; id: number | null; lot?: number }
	| { name: "ingredients"; tab: IngredientsTab }
	| { name: "settings" };

export type IngredientsTab = "ingredients" | "strains";

/** Anything unrecognised is home: a stale bookmark should land somewhere useful. */
export function parse(hash: string): Route {
	const path = hash.replace(/^#/, "").replace(/\/+$/, "");
	if (path === "/settings") return { name: "settings" };
	const batch = /^\/batch\/(\d+)$/.exec(path);
	if (batch) return { name: "batch", id: Number(batch[1]) };
	// 5b: `#/compare/3,7,12`, in pick order; `#/compare` is an empty pick.
	const compare = /^\/compare(?:\/(\d+(?:,\d+)*))?$/.exec(path);
	if (compare) return { name: "compare", ids: compare[1] ? compare[1].split(",").map(Number) : [] };
	// U1: `#/query/lot/12` is a fresh query preset to one lot — "which batches used it".
	const lot = /^\/query\/lot\/(\d+)$/.exec(path);
	if (lot) return { name: "query", id: null, lot: Number(lot[1]) };
	// 5c: `#/query` is a fresh filter, `#/query/4` saved query 4.
	const query = /^\/query(?:\/(\d+))?$/.exec(path);
	if (query) return { name: "query", id: query[1] ? Number(query[1]) : null };
	// U1: `#/ingredients`, `#/ingredients/strains`.
	const ingredients = /^\/ingredients(?:\/(strains))?$/.exec(path);
	if (ingredients) return { name: "ingredients", tab: ingredients[1] ? "strains" : "ingredients" };
	return { name: "home" };
}

export function href(route: Route): string {
	switch (route.name) {
		case "home":
			return "#/";
		case "batch":
			return `#/batch/${route.id}`;
		case "compare":
			return route.ids.length ? `#/compare/${route.ids.join(",")}` : "#/compare";
		case "query":
			if (route.id === null && route.lot !== undefined) return `#/query/lot/${route.lot}`;
			return route.id === null ? "#/query" : `#/query/${route.id}`;
		case "ingredients":
			return route.tab === "strains" ? "#/ingredients/strains" : "#/ingredients";
		case "settings":
			return "#/settings";
	}
}
