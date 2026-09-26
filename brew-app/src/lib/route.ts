/**
 * Phase 4f. Hash routing in a few lines, no router dependency: four screens and their
 * parameters don't earn one. Hash rather than path because Tauri serves a single file.
 */

export type Route =
	| { name: "home" }
	| { name: "batch"; id: number }
	| { name: "compare"; ids: number[] }
	| { name: "settings" };

/** Anything unrecognised is home: a stale bookmark should land somewhere useful. */
export function parse(hash: string): Route {
	const path = hash.replace(/^#/, "").replace(/\/+$/, "");
	if (path === "/settings") return { name: "settings" };
	const batch = /^\/batch\/(\d+)$/.exec(path);
	if (batch) return { name: "batch", id: Number(batch[1]) };
	// 5b: `#/compare/3,7,12`, in pick order; `#/compare` is an empty pick.
	const compare = /^\/compare(?:\/(\d+(?:,\d+)*))?$/.exec(path);
	if (compare) return { name: "compare", ids: compare[1] ? compare[1].split(",").map(Number) : [] };
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
		case "settings":
			return "#/settings";
	}
}
