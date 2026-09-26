/**
 * Phase 4f. Hash routing in a few lines, no router dependency: three screens and one
 * parameter don't earn one. Hash rather than path because Tauri serves a single file.
 */

export type Route = { name: "home" } | { name: "batch"; id: number } | { name: "settings" };

/** Anything unrecognised is home: a stale bookmark should land somewhere useful. */
export function parse(hash: string): Route {
	const path = hash.replace(/^#/, "").replace(/\/+$/, "");
	if (path === "/settings") return { name: "settings" };
	const batch = /^\/batch\/(\d+)$/.exec(path);
	if (batch) return { name: "batch", id: Number(batch[1]) };
	return { name: "home" };
}

export function href(route: Route): string {
	switch (route.name) {
		case "home":
			return "#/";
		case "batch":
			return `#/batch/${route.id}`;
		case "settings":
			return "#/settings";
	}
}
