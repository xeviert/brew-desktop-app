import { describe, expect, it } from "vitest";
import { href, parse } from "./route";

describe("parse", () => {
	it("reads the three screens", () => {
		expect(parse("#/")).toEqual({ name: "home" });
		expect(parse("#/batch/17")).toEqual({ name: "batch", id: 17 });
		expect(parse("#/settings")).toEqual({ name: "settings" });
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
		for (const r of [{ name: "home" }, { name: "batch", id: 3 }, { name: "settings" }] as const) {
			expect(parse(href(r))).toEqual(r);
		}
	});
});
